//! Native loading is the editor's sole unsafe boundary. Implementations in this
//! local developer directory must obey the experimental plugin ABI.
#![allow(unsafe_code)]
pub fn start() -> Option<velk_plugin_host::Runtime> {
    let directory = match velk_plugin_host::default_directory() {
        Ok(directory) => directory,
        Err(error) => {
            eprintln!("velk: plugins unavailable: {error}");
            return None;
        }
    };
    // SAFETY: plugins here are explicitly trusted local native code. v1
    // plugins obey borrowed-call and semantic join-before-release lifetimes.
    match unsafe { velk_plugin_host::Runtime::start(directory) } {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            eprintln!("velk: plugins unavailable: {error}");
            None
        }
    }
}
