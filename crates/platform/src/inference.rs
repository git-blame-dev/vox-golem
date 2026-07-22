#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InferencePolicy {
    #[default]
    Auto,
    Cuda,
    Cpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActualInferenceProvider {
    Cuda,
    Cpu,
    /// CUDA was requested, but the runtime does not verify that the backend
    /// actually initialized it.
    RequestedCuda,
    AttachedUnknown,
}
