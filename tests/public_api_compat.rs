//! Source compatibility with the published 0.1.0 crate: an external
//! consumer's exhaustive struct literal of `Assignment` must keep compiling.
//! (`ExperimentData` and `Exposure` gained wire fields in 0.2.0; see the
//! CHANGELOG.)

use std::collections::HashMap;

use absmartly::Assignment;

#[test]
fn assignment_literal_from_0_1_0_compiles() {
    let assignment = Assignment {
        id: 1,
        iteration: 2,
        full_on_variant: 0,
        unit_type: Some("session_id".into()),
        variant: 1,
        overridden: false,
        assigned: true,
        exposed: false,
        eligible: true,
        full_on: false,
        custom: false,
        audience_mismatch: false,
        traffic_split: Some(vec![0.0, 1.0]),
        variables: Some(HashMap::new()),
        attrs_seq: 0,
    };
    assert_eq!(assignment.variant, 1);
}
