//! One verified image can supply many independent plugin generations.
use super::*;
use libloading::Library;
pub(super) struct Code {
    // Dropped before the private copy is removed (required on Windows).
    library: Library,
    pub path: PathBuf,
    _copy: tempfile::TempDir,
    pub apis: Vec<PluginApi>,
    pub resident: bool,
}
fn validate(api: &PluginApi) -> Result<String, String> {
    if api.abi_version != ABI_VERSION
        || api.struct_size as usize != std::mem::size_of::<PluginApi>()
    {
        return Err("incompatible plugin API layout".into());
    }
    if api.flags & !RESIDENT_CODE != 0 {
        return Err("unknown plugin lifetime flags".into());
    }
    let id = metadata(&api.id)?;
    if parse_plugin_id(&id).is_none() {
        return Err("invalid plugin ID in factory metadata".into());
    }
    metadata(&api.version)?;
    if api.create.is_none()
        || api.destroy.is_none()
        || api.describe.is_none()
        || api.hook_count.is_none()
        || api.hook_info.is_none()
        || api.get_hook.is_none()
    {
        return Err("missing lifecycle or registry function".into());
    }
    Ok(id)
}
impl Code {
    #[tracing::instrument(level = "debug", skip_all, fields(plugin = %spec.id))]
    pub unsafe fn load(
        directory: &std::path::Path,
        spec: &PluginSpec,
        cached: Option<Arc<Self>>,
    ) -> Result<Arc<Self>, String> {
        let source = directory.join(&spec.library);
        // Hash cached sources too: an old mapped image must not hide a broken or
        // tampered manifest artifact. A new image is verified after copying.
        if let Some(code) = cached {
            verify(&source, &spec.digest)?;
            return Ok(code);
        }
        let copy = tempfile::Builder::new()
            .prefix("velk-plugin-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let path = copy
            .path()
            .join(source.file_name().ok_or("library has no filename")?);
        std::fs::copy(&source, &path).map_err(|e| format!("{}: {e}", source.display()))?;
        verify(&path, &spec.digest)?;
        let mut code = Self {
            library: unsafe { Library::new(&path) }.map_err(|e| e.to_string())?,
            path,
            _copy: copy,
            apis: vec![],
            resident: false,
        };
        if spec.pack {
            let entry = unsafe { code.library.get::<PackEntry>(PACK_ENTRY_SYMBOL) }
                .map_err(|e| e.to_string())?;
            let mut output = std::mem::MaybeUninit::<PluginPackApi>::uninit();
            let status = unsafe {
                entry(
                    ABI_VERSION,
                    output.as_mut_ptr(),
                    std::mem::size_of::<PluginPackApi>(),
                )
            };
            if status != OK {
                return Err(format!("pack ABI query failed ({status})"));
            }
            let pack = unsafe { output.assume_init() };
            if pack.abi_version != ABI_VERSION
                || pack.struct_size as usize != std::mem::size_of::<PluginPackApi>()
                || pack.member_count == 0
                || pack.member_count > MAX_PACK_MEMBERS
                || pack.member.is_none()
            {
                return Err("incompatible plugin pack layout or member count".into());
            }
            let mut ids = BTreeSet::new();
            for index in 0..pack.member_count {
                let mut output = std::mem::MaybeUninit::<PluginApi>::uninit();
                let status = unsafe {
                    pack.member.unwrap()(
                        index,
                        ABI_VERSION,
                        output.as_mut_ptr(),
                        std::mem::size_of::<PluginApi>(),
                    )
                };
                if status != OK {
                    return Err(format!("pack member {index} query failed ({status})"));
                }
                let api = unsafe { output.assume_init() };
                if !ids.insert(validate(&api)?) {
                    return Err("duplicate plugin ID in pack".into());
                }
                code.apis.push(api);
            }
        } else {
            let entry =
                unsafe { code.library.get::<Entry>(ENTRY_SYMBOL) }.map_err(|e| e.to_string())?;
            let mut output = std::mem::MaybeUninit::<PluginApi>::uninit();
            let status = unsafe {
                entry(
                    ABI_VERSION,
                    output.as_mut_ptr(),
                    std::mem::size_of::<PluginApi>(),
                )
            };
            if status != OK {
                return Err(format!("ABI query failed with status {status}"));
            }
            let api = unsafe { output.assume_init() };
            validate(&api)?;
            code.apis.push(api);
        }
        code.resident = code.apis.iter().any(|api| api.flags & RESIDENT_CODE != 0);
        Ok(Arc::new(code))
    }
}
fn verify(path: &std::path::Path, digest: &str) -> Result<(), String> {
    let mut hash = blake3::Hasher::new();
    hash.update_reader(std::fs::File::open(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if hash.finalize().to_hex().as_str() != digest {
        return Err("library digest mismatch".into());
    }
    Ok(())
}
