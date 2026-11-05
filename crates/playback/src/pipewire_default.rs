//! The sink PipeWire plays on by default, which cpal's stand-in for it
//! follows without saying which one it is.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use pipewire::context::ContextRc;
use pipewire::core::PW_ID_CORE;
use pipewire::main_loop::MainLoopRc;
use pipewire::metadata::{Metadata, MetadataListener};
use pipewire::types::ObjectType;

/// How long PipeWire is given to answer, as one that hangs never does.
const ANSWER: Duration = Duration::from_secs(1);

/// The node name of PipeWire's default sink. PipeWire stays set up as long
/// as cpal's host on it, `_pipewire`, is there.
pub(crate) fn default_sink(_pipewire: &cpal::Host) -> Option<String> {
    pipewire::init();
    let main_loop = MainLoopRc::new(None).ok()?;
    let context = ContextRc::new(&main_loop, None).ok()?;
    let core = context.connect_rc(None).ok()?;
    let registry = core.get_registry_rc().ok()?;
    let sink = Rc::new(RefCell::new(None));
    let bound: Rc<RefCell<Option<(Metadata, MetadataListener)>>> = Rc::default();
    // Answered once every object there is has been told of, then once the
    // defaults have.
    let last_asked = Rc::new(Cell::new(core.sync(0).ok()?));
    let _objects = registry
        .add_listener_local()
        .global({
            let (registry, core) = (registry.clone(), core.clone());
            let (sink, bound, last_asked) = (sink.clone(), bound.clone(), last_asked.clone());
            move |object| {
                let is_defaults = matches!(object.type_, ObjectType::Metadata)
                    && object
                        .props
                        .is_some_and(|props| props.get("metadata.name") == Some("default"));
                if !is_defaults {
                    return;
                }
                let Ok(defaults) = registry.bind::<Metadata, _>(object) else {
                    return;
                };
                let sink = sink.clone();
                let listener = defaults
                    .add_listener_local()
                    .property(move |_, key, _, value| {
                        if key == Some("default.audio.sink") {
                            *sink.borrow_mut() = value.and_then(node_name).map(str::to_owned);
                        }
                        0
                    })
                    .register();
                bound.replace(Some((defaults, listener)));
                if let Ok(asked) = core.sync(0) {
                    last_asked.set(asked);
                }
            }
        })
        .register();
    let _answers = core
        .add_listener_local()
        .done({
            let (main_loop, last_asked) = (main_loop.clone(), last_asked.clone());
            move |id, answered| {
                if id == PW_ID_CORE && answered == last_asked.get() {
                    main_loop.quit();
                }
            }
        })
        .register();
    let timeout = main_loop.loop_().add_timer({
        let main_loop = main_loop.clone();
        move |_| main_loop.quit()
    });
    timeout
        .update_timer(Some(ANSWER), None)
        .into_result()
        .ok()?;
    main_loop.run();
    sink.take()
}

/// The node a default names, as `{"name":"alsa_output.usb"}` does.
fn node_name(value: &str) -> Option<&str> {
    let (_, after_key) = value.split_once("\"name\"")?;
    let (_, quoted) = after_key.split_once('"')?;
    quoted.split_once('"').map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_names_its_node() {
        let named = [
            (r#"{"name":"alsa_output.usb"}"#, Some("alsa_output.usb")),
            (
                r#"{ "name": "bluez_output.AA_BB.1" }"#,
                Some("bluez_output.AA_BB.1"),
            ),
            ("{}", None),
            ("", None),
        ];
        for (value, node) in named {
            assert_eq!(node_name(value), node, "{value}");
        }
    }
}
