//! Memory as Task Manager reports it ("Memory" column = private working set).

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2};
use windows::Win32::System::Threading::GetCurrentProcess;

pub struct Sample {
    pub private_ws: f64,
    pub private_bytes: f64,
    pub working_set: f64,
}

pub fn sample() -> Sample {
    let mut c = PROCESS_MEMORY_COUNTERS_EX2 { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32, ..Default::default() };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb);
    }
    let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
    Sample { private_ws: mb(c.PrivateWorkingSetSize), private_bytes: mb(c.PrivateUsage), working_set: mb(c.WorkingSetSize) }
}
