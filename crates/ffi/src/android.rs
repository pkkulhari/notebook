//! Android-only glue: the JNI call iroh needs, and logcat logging.
use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JClass, JObject};
use std::sync::Once;

static CONTEXT: Once = Once::new();

/// Gives iroh the JVM and the application context so it can read the
/// network's DNS servers; without them it falls back to public resolvers.
/// Installing twice would assert, hence the `Once`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_pkkulhari_notebook_AndroidContext_install<'caller>(
    mut env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    context: JObject<'caller>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        if CONTEXT.is_completed() {
            return Ok(());
        }
        let vm = env.get_java_vm()?;
        let context = env.new_global_ref(&context)?;
        CONTEXT.call_once(|| unsafe {
            // Both stay valid for the life of the process: the VM always, and the
            // context because its global reference is never released.
            iroh::dns::install_android_jni_context(vm.get_raw().cast(), context.into_raw().cast());
        });
        Ok(())
    })
    .resolve::<ThrowRuntimeExAndDefault>()
}

/// Sends Rust logs and panics to logcat under the tag "NotebookRust".
#[cfg(feature = "logcat")]
pub fn init_logging() {
    static LOGGING: Once = Once::new();
    LOGGING.call_once(|| {
        use tracing::Level;
        use tracing_subscriber::{
            Layer, filter::Targets, layer::SubscriberExt, util::SubscriberInitExt,
        };
        let filter = Targets::new()
            .with_default(Level::INFO)
            // Loro logs block diagnostics at INFO on every save.
            .with_target("loro_internal", Level::WARN)
            .with_target("swarm_discovery", Level::DEBUG)
            .with_target("iroh_mdns_address_lookup", Level::DEBUG)
            .with_target("netwatch", Level::DEBUG)
            .with_target("n0_dns_resolver", Level::DEBUG);
        let _ = tracing_subscriber::registry()
            .with(paranoid_android::layer("NotebookRust").with_filter(filter))
            .try_init();
        std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
    });
}
