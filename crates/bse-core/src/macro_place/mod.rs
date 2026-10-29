pub mod cost;
pub mod ordering;
pub mod packer;
pub mod topology;

pub use cost::{
    compute_bridge_demand, cross_cost, pick_best_cross_pair, pure_orientation_bridge_cost,
};
pub use ordering::{best_assignments, conn_order, MacroAssignment};
pub use packer::{place_components, PackResult, PlaceOptions};
pub use topology::{
    build_topology_cache, compute_exact_1d_positions, infer_optimal_btn_halves, TopologyCache,
};
