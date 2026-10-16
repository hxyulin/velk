//! Typed safe registrations over a named C-table discovery ABI. Binding and table
//! storage stay in their creating library until the plugin is destroyed.
use super::{fixed, guard};
use crate::{LanguageServer, Plugin, api::*};
use std::{any::Any, ffi::c_void};

pub struct Registrar<P: Plugin> {
    plugin: *mut P,
    pub(super) entries: Vec<Registration>,
}
pub(super) struct Registration {
    pub info: HookInfo,
    table: Box<dyn Any>,
    _binding: Box<dyn Any>,
    copy: unsafe fn(&dyn Any, *mut c_void),
}
pub struct Binding<P, F> {
    pub plugin: *mut P,
    pub handler: F,
}
pub trait Provider<P: Plugin, F> {
    fn install(self, reg: &mut Registrar<P>, handler: F) -> Result<(), String>;
}
impl<P: Plugin> Registrar<P> {
    pub(super) fn new(plugin: *mut P) -> Self {
        Self {
            plugin,
            entries: vec![],
        }
    }
    pub fn provide<C, F>(&mut self, contract: C, handler: F) -> Result<(), String>
    where
        C: Provider<P, F>,
    {
        contract.install(self, handler)
    }
    pub fn on<N: AsRef<str>, F>(&mut self, event: Event<N>, handler: F) -> Result<(), String>
    where
        F: FnMut(&mut P, &[u8]) -> Result<(), String> + 'static,
    {
        self.provide(event, handler)
    }
    /// Advanced contract packages can supply new C-compatible tables without
    /// changing Plugin, Registrar, or the root ABI.
    /// # Safety
    /// T must be a repr(C), Copy table of C function pointers/opaque contexts.
    /// Its layout/signatures must exactly match name/contract/version. All
    /// contexts must be owned by binding or remain valid until destruction.
    pub unsafe fn register_raw<T: Copy + 'static, H: 'static>(
        &mut self,
        name: &str,
        contract: &str,
        version: u32,
        kind: u32,
        table: T,
        binding: H,
    ) -> Result<(), String> {
        if self.entries.len() >= MAX_HOOKS
            || !valid_name(name)
            || !valid_name(contract)
            || version == 0
            || !(REQUEST..=CONTRIBUTION).contains(&kind)
            || self.entries.iter().any(|r| {
                name == std::str::from_utf8(
                    &r.info.name[..r.info.name.iter().position(|b| *b == 0).unwrap()],
                )
                .unwrap()
            })
        {
            return Err(format!("invalid or duplicate hook registration: {name}"));
        }
        if std::mem::size_of::<T>() == 0 || std::mem::size_of::<T>() > 4096 {
            return Err("invalid hook table size".into());
        }
        unsafe fn copy<T: Copy + 'static>(table: &dyn Any, output: *mut c_void) {
            // SAFETY: constructor and query match this concrete table and layout.
            unsafe {
                output
                    .cast::<T>()
                    .write(*table.downcast_ref::<T>().unwrap());
            }
        }
        self.entries.push(Registration {
            info: HookInfo {
                name: fixed(name),
                contract: fixed(contract),
                version,
                kind,
                table_size: std::mem::size_of::<T>(),
            },
            table: Box::new(table),
            _binding: Box::new(binding),
            copy: copy::<T>,
        });
        Ok(())
    }
    /// Access for advanced contract packages that generate their own safe adapter.
    /// # Safety
    /// The returned pointer is exclusive only during serialized host calls. Never
    /// retain it past plugin destruction or dereference it on a producer thread.
    pub unsafe fn plugin_context(&self) -> *mut P {
        self.plugin
    }
    pub(super) fn binding<F: 'static>(&self, handler: F) -> Box<Binding<P, F>> {
        Box::new(Binding {
            plugin: self.plugin,
            handler,
        })
    }
}
fn valid_name(s: &str) -> bool {
    s.len() < 128
        && s.contains('.')
        && s.split('.').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        })
}

#[cfg(feature = "markup")]
pub struct RenderMarkdown;
#[cfg(feature = "markup")]
impl<P, F> Provider<P, F> for RenderMarkdown
where
    P: Plugin,
    F: FnMut(&mut P, &str) -> crate::markup::Document + 'static,
{
    fn install(self, reg: &mut Registrar<P>, handler: F) -> Result<(), String> {
        let mut binding = reg.binding(handler);
        let table = MarkupApi {
            context: (&mut *binding as *mut Binding<P, F>).cast(),
            render: super::render_markdown::<P, F>,
        };
        // SAFETY: built-in contract binds this exact table/signature and owns context.
        unsafe { reg.register_raw(MARKUP_HOOK, MARKUP_CONTRACT, 1, REQUEST, table, binding) }
    }
}
pub struct LanguageDefinition;
impl<P, F> Provider<P, F> for LanguageDefinition
where
    P: Plugin,
    F: FnMut(&mut P, &str) -> Result<Option<LanguageServer>, String> + 'static,
{
    fn install(self, reg: &mut Registrar<P>, handler: F) -> Result<(), String> {
        let mut binding = reg.binding(handler);
        let table = LanguageApi {
            context: (&mut *binding as *mut Binding<P, F>).cast(),
            query: super::language_server::query::<P, F>,
        };
        unsafe {
            reg.register_raw(
                LANGUAGE_HOOK,
                LANGUAGE_CONTRACT,
                1,
                CONTRIBUTION,
                table,
                binding,
            )
        }
    }
}
pub struct SemanticSessions;
impl<P, F> Provider<P, F> for SemanticSessions
where
    P: Plugin,
    F: FnMut(&mut P, LanguageServer) -> Box<dyn crate::semantic::Backend> + 'static,
{
    fn install(self, reg: &mut Registrar<P>, handler: F) -> Result<(), String> {
        let mut binding = reg.binding(handler);
        let table = SemanticApi {
            context: (&mut *binding as *mut Binding<P, F>).cast(),
            create: super::semantic::create::<P, F>,
            input: super::semantic::input,
            destroy: super::semantic::destroy,
        };
        unsafe { reg.register_raw(SEMANTIC_HOOK, SEMANTIC_CONTRACT, 1, SERVICE, table, binding) }
    }
}
pub struct Request<N: AsRef<str>>(pub N);
impl<P, F, N: AsRef<str>> Provider<P, F> for Request<N>
where
    P: Plugin,
    F: FnMut(&mut P, &[u8]) -> Result<Vec<u8>, String> + 'static,
{
    fn install(self, reg: &mut Registrar<P>, handler: F) -> Result<(), String> {
        let mut binding = reg.binding(handler);
        let table = BytesApi {
            context: (&mut *binding as *mut Binding<P, F>).cast(),
            call: call::<P, F>,
        };
        unsafe { reg.register_raw(self.0.as_ref(), BYTES_CONTRACT, 1, REQUEST, table, binding) }
    }
}
pub struct Event<N: AsRef<str>>(pub N);
impl<P, F, N: AsRef<str>> Provider<P, F> for Event<N>
where
    P: Plugin,
    F: FnMut(&mut P, &[u8]) -> Result<(), String> + 'static,
{
    fn install(self, reg: &mut Registrar<P>, handler: F) -> Result<(), String> {
        let mut binding = reg.binding(handler);
        let table = EventApi {
            context: (&mut *binding as *mut Binding<P, F>).cast(),
            notify: notify::<P, F>,
        };
        unsafe { reg.register_raw(self.0.as_ref(), EVENT_CONTRACT, 1, EVENT, table, binding) }
    }
}
unsafe fn payload<'a>(input: Bytes) -> Result<&'a [u8], i32> {
    if input.length > MAX_INPUT_BYTES || (input.length > 0 && input.data.is_null()) {
        return Err(INVALID);
    }
    if input.length == 0 {
        return Ok(&[]);
    }
    Ok(unsafe { std::slice::from_raw_parts(input.data, input.length) })
}
unsafe extern "C" fn call<P, F>(
    context: *mut c_void,
    input: Bytes,
    sink: ByteSink,
    reply: *mut c_void,
) -> i32
where
    P: Plugin,
    F: FnMut(&mut P, &[u8]) -> Result<Vec<u8>, String> + 'static,
{
    if context.is_null() {
        return INVALID;
    }
    guard(|| {
        let Ok(input) = (unsafe { payload(input) }) else {
            return INVALID;
        };
        let binding = unsafe { &mut *context.cast::<Binding<P, F>>() };
        let result = (binding.handler)(unsafe { &mut *binding.plugin }, input);
        let (bytes, status) = match result {
            Ok(b) => (b, OK),
            Err(e) => (e.into_bytes(), INVALID),
        };
        if bytes.len() > MAX_OUTPUT_BYTES {
            return INVALID;
        }
        unsafe {
            sink(
                reply,
                Bytes {
                    data: bytes.as_ptr(),
                    length: bytes.len(),
                },
                status,
            )
        }
    })
}
unsafe extern "C" fn notify<P, F>(context: *mut c_void, input: Bytes) -> i32
where
    P: Plugin,
    F: FnMut(&mut P, &[u8]) -> Result<(), String> + 'static,
{
    if context.is_null() {
        return INVALID;
    }
    guard(|| {
        let Ok(input) = (unsafe { payload(input) }) else {
            return INVALID;
        };
        let binding = unsafe { &mut *context.cast::<Binding<P, F>>() };
        if (binding.handler)(unsafe { &mut *binding.plugin }, input).is_ok() {
            OK
        } else {
            INVALID
        }
    })
}
pub(super) unsafe extern "C" fn count<P: Plugin>(instance: *mut c_void) -> usize {
    if instance.is_null() {
        return 0;
    }
    unsafe {
        (&*instance.cast::<super::Instance<P>>())
            .registry
            .entries
            .len()
    }
}
pub(super) unsafe extern "C" fn info<P: Plugin>(
    instance: *mut c_void,
    index: usize,
    out: *mut HookInfo,
) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID;
    }
    guard(|| {
        let instance = unsafe { &*instance.cast::<super::Instance<P>>() };
        let Some(entry) = instance.registry.entries.get(index) else {
            return NOT_FOUND;
        };
        unsafe {
            out.write(entry.info);
        }
        OK
    })
}
pub(super) unsafe extern "C" fn get<P: Plugin>(
    instance: *mut c_void,
    index: usize,
    contract: Bytes,
    version: u32,
    size: usize,
    out: *mut c_void,
) -> i32 {
    if instance.is_null() || out.is_null() {
        return INVALID;
    }
    guard(|| {
        let Ok(contract) = (unsafe { payload(contract) }) else {
            return INVALID;
        };
        let instance = unsafe { &*instance.cast::<super::Instance<P>>() };
        let Some(entry) = instance.registry.entries.get(index) else {
            return NOT_FOUND;
        };
        let expected =
            &entry.info.contract[..entry.info.contract.iter().position(|b| *b == 0).unwrap()];
        if contract != expected || version != entry.info.version || size != entry.info.table_size {
            return INCOMPATIBLE;
        }
        unsafe {
            (entry.copy)(entry.table.as_ref(), out);
        }
        OK
    })
}

/// Typed command handler over the existing bounded JSON request contract.
pub struct CommandHandler<N> {
    pub name: N,
}
impl<P, F, N> Provider<P, F> for CommandHandler<N>
where
    P: Plugin,
    N: AsRef<str>,
    F: FnMut(&mut P, crate::commands::Context) -> Result<crate::commands::Outcome, String>
        + 'static,
{
    fn install(self, reg: &mut Registrar<P>, mut handler: F) -> Result<(), String> {
        reg.provide(Request(self.name), move |plugin: &mut P, input: &[u8]| {
            let context = serde_json::from_slice(input).map_err(|e| e.to_string())?;
            let outcome = handler(plugin, context)?;
            outcome.validate()?;
            serde_json::to_vec(&outcome).map_err(|e| e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Sample;
    impl Plugin for Sample {
        const ID: &'static str = "test.registry";
        const VERSION: &'static str = "1";
        fn describe(&mut self) -> String {
            String::new()
        }
        fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
            reg.provide(Request("test.custom"), |_: &mut Self, b: &[u8]| {
                Ok(b.to_vec())
            })
        }
    }
    #[test]
    fn query_rejects_wrong_contract_version_and_layout_before_writing_output() {
        unsafe {
            let mut instance = std::ptr::null_mut();
            assert_eq!(super::super::create::<Sample>(&mut instance), OK);
            let mut output = std::mem::MaybeUninit::<BytesApi>::uninit();
            let bytes = |s: &str| Bytes {
                data: s.as_ptr(),
                length: s.len(),
            };
            assert_eq!(
                get::<Sample>(
                    instance,
                    0,
                    bytes(EVENT_CONTRACT),
                    1,
                    std::mem::size_of::<BytesApi>(),
                    output.as_mut_ptr().cast()
                ),
                INCOMPATIBLE
            );
            assert_eq!(
                get::<Sample>(
                    instance,
                    0,
                    bytes(BYTES_CONTRACT),
                    2,
                    std::mem::size_of::<BytesApi>(),
                    output.as_mut_ptr().cast()
                ),
                INCOMPATIBLE
            );
            assert_eq!(
                get::<Sample>(
                    instance,
                    0,
                    bytes(BYTES_CONTRACT),
                    1,
                    1,
                    output.as_mut_ptr().cast()
                ),
                INCOMPATIBLE
            );
            assert_eq!(
                get::<Sample>(
                    instance,
                    count::<Sample>(instance),
                    bytes(BYTES_CONTRACT),
                    1,
                    std::mem::size_of::<BytesApi>(),
                    output.as_mut_ptr().cast()
                ),
                NOT_FOUND
            );
            assert_eq!(
                get::<Sample>(
                    instance,
                    0,
                    bytes(BYTES_CONTRACT),
                    1,
                    std::mem::size_of::<BytesApi>(),
                    output.as_mut_ptr().cast()
                ),
                OK
            );
            assert_eq!(super::super::destroy::<Sample>(instance), OK);
        }
    }
    #[test]
    fn duplicate_names_and_malformed_registration_names_are_rejected() {
        let mut plugin = Sample;
        let mut registry = Registrar::new(&mut plugin);
        registry
            .provide(Request("test.same"), |_: &mut Sample, b: &[u8]| {
                Ok(b.to_vec())
            })
            .unwrap();
        assert!(
            registry
                .on(Event("test.same"), |_: &mut Sample, _: &[u8]| Ok(()))
                .is_err()
        );
        registry
            .provide(
                Request(format!("test.{}", "dynamic")),
                |_: &mut Sample, _: &[u8]| Ok(vec![]),
            )
            .unwrap();
        for name in ["", "unscoped", "test..bad", "test.with space"] {
            assert!(
                registry
                    .provide(Request(name), |_: &mut Sample, _: &[u8]| Ok(vec![]))
                    .is_err()
            );
        }
    }
}
