//! Nonresident pack with independent state and one deliberately failing factory.
use velk_plugin_sdk::{Plugin, Registrar, hooks};
#[derive(Default)]
struct First(u8);
#[derive(Default)]
struct Second(u8);
macro_rules! plugin {
    ($ty:ty, $id:literal) => {
        impl Plugin for $ty {
            const ID: &'static str = $id;
            const VERSION: &'static str = "1";
            fn describe(&mut self) -> String {
                Self::ID.into()
            }
            fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
                reg.provide(
                    hooks::Request("velk.lifecycle.snapshot.v1"),
                    |this: &mut Self, _: &[u8]| {
                        if this.0 == 250 {
                            return Err("expected snapshot failure".into());
                        }
                        Ok(vec![this.0])
                    },
                )?;
                reg.provide(
                    hooks::Request("velk.lifecycle.restore.v1"),
                    |this: &mut Self, bytes: &[u8]| {
                        if bytes == [251] {
                            return Err("expected restore failure".into());
                        }
                        this.0 = *bytes.first().ok_or("missing state")?;
                        Ok(vec![])
                    },
                )?;
                reg.provide(
                    hooks::Request("test.pack.set"),
                    |this: &mut Self, bytes: &[u8]| {
                        this.0 = *bytes.first().ok_or("missing state")?;
                        Ok(vec![])
                    },
                )?;
                reg.provide(
                    hooks::Request("test.pack.increment"),
                    |this: &mut Self, _: &[u8]| {
                        this.0 += 1;
                        Ok(vec![this.0])
                    },
                )
            }
        }
    };
}
plugin!(First, "test.pack.first");
plugin!(Second, "test.pack.second");
struct Fails;
impl Default for Fails {
    fn default() -> Self {
        panic!("expected pack factory failure")
    }
}
impl Plugin for Fails {
    const ID: &'static str = "test.pack.fails";
    const VERSION: &'static str = "1";
    fn describe(&mut self) -> String {
        unreachable!()
    }
}
velk_plugin_sdk::export_plugin_pack!(First, Second, Fails);
