pub mod cow_memory_creator;
mod signal_handler;
pub mod wasm_executor;
pub mod wasm_utils;
pub mod wasmtime_embedder;

use ic_interfaces::execution_environment::{ExecutionParameters, HypervisorError, InstanceStats};
use ic_replicated_state::{ExecutionState, Global, NumWasmPages, PageIndex};
use ic_sys::PageBytes;
use ic_system_api::{ApiType, StaticSystemState, SystemStateAccessorDirect};
use ic_types::{ingress::WasmResult, methods::FuncRef, NumBytes, NumInstructions};
use serde::{Deserialize, Serialize};
pub use wasmtime_embedder::{WasmtimeEmbedder, WasmtimeMemoryCreator};

pub struct WasmExecutionInput {
    pub api_type: ApiType,
    pub static_system_state: StaticSystemState,
    pub canister_current_memory_usage: NumBytes,
    pub execution_parameters: ExecutionParameters,
    pub func_ref: FuncRef,
    pub execution_state: ExecutionState,
    pub system_state_accessor: SystemStateAccessorDirect,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WasmExecutionOutput {
    pub wasm_result: Result<Option<WasmResult>, HypervisorError>,
    pub num_instructions_left: NumInstructions,
    pub instance_stats: InstanceStats,
}

pub struct InstanceRunResult {
    pub dirty_pages: Vec<PageIndex>,
    pub stable_memory_size: NumWasmPages,
    pub stable_memory_dirty_pages: Vec<(PageIndex, PageBytes)>,
    pub exported_globals: Vec<Global>,
}

pub trait LinearMemory {
    fn as_ptr(&self) -> *mut libc::c_void;
}

pub trait ICMemoryCreator {
    type Mem: LinearMemory;

    fn new_memory(
        &self,
        mem_size: usize,
        guard_size: usize,
        instance_heap_offset: usize,
        min_pages: u32,
        max_pages: Option<u32>,
    ) -> Self::Mem;
}
