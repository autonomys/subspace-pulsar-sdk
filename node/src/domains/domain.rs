use derivative::Derivative;
use sdk_utils::DestructorSet;

/// Node structure
#[derive(Derivative)]
#[derivative(Debug)]
#[must_use = "Domain should be closed"]
pub struct Domain {
    _destructors: DestructorSet
}
