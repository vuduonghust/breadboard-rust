pub mod decoupling;
pub mod pin_swap;
pub mod special;
pub mod special_order;

pub use decoupling::{
    base_net_of, is_pwr_tag, is_pwr_tied_tag, nc_tag, pwr_aux_tag, pwr_direct_tag, pwr_tag,
    setup_macro_and_decoupling, DiscreteContext, HolePin,
};
pub use pin_swap::optimize_symmetric_pins;
pub use special::place_special_components;
pub use special_order::{
    apply_frozen_specials, build_special_order, can_direct_plug_rail, can_straight_rail,
    compute_net_stats, find_series_partners, update_net_centers, IcSnapshot, SpecialOptions,
};
