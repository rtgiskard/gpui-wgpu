pub use gpui::layer_shell::*;

use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

pub(crate) fn wayland_layer(layer: Layer) -> zwlr_layer_shell_v1::Layer {
    match layer {
        Layer::Background => zwlr_layer_shell_v1::Layer::Background,
        Layer::Bottom => zwlr_layer_shell_v1::Layer::Bottom,
        Layer::Top => zwlr_layer_shell_v1::Layer::Top,
        Layer::Overlay => zwlr_layer_shell_v1::Layer::Overlay,
    }
}

pub(crate) fn wayland_anchor(anchor: Anchor) -> zwlr_layer_surface_v1::Anchor {
    zwlr_layer_surface_v1::Anchor::from_bits_truncate(anchor.bits())
}

/// On-demand focus requires layer-surface version 4; older compositors raise a fatal
/// `invalid_keyboard_interactivity` error for it, so it degrades to no keyboard focus
/// rather than an exclusive grab.
pub(crate) fn wayland_keyboard_interactivity(
    value: KeyboardInteractivity,
    version: u32,
) -> zwlr_layer_surface_v1::KeyboardInteractivity {
    match value {
        KeyboardInteractivity::None => zwlr_layer_surface_v1::KeyboardInteractivity::None,
        KeyboardInteractivity::Exclusive => zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
        KeyboardInteractivity::OnDemand if version < ON_DEMAND_SINCE => {
            log::warn!("on-demand keyboard focus is unsupported by the compositor; using none");
            zwlr_layer_surface_v1::KeyboardInteractivity::None
        }
        KeyboardInteractivity::OnDemand => zwlr_layer_surface_v1::KeyboardInteractivity::OnDemand,
    }
}

const ON_DEMAND_SINCE: u32 = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_demand_focus_degrades_only_before_version_4() {
        use zwlr_layer_surface_v1::KeyboardInteractivity as Wire;
        for version in 1..=3 {
            assert_eq!(
                wayland_keyboard_interactivity(KeyboardInteractivity::OnDemand, version),
                Wire::None
            );
            assert_eq!(
                wayland_keyboard_interactivity(KeyboardInteractivity::Exclusive, version),
                Wire::Exclusive
            );
        }
        assert_eq!(
            wayland_keyboard_interactivity(KeyboardInteractivity::OnDemand, 4),
            Wire::OnDemand
        );
    }
}
