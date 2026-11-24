//! Process memory as Task Manager shows it: "Memory" column = private working set,
//! plus Private Bytes (commit) and the full working set.

use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

pub fn report(label: &str) {
    let mut c: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut _, c.cb) };
    if ok == 0 {
        return;
    }
    let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
    eprintln!(
        "[mem] {label}: private bytes {:.1} MB, working set {:.1} MB, peak WS {:.1} MB",
        mb(c.PrivateUsage),
        mb(c.WorkingSetSize),
        mb(c.PeakWorkingSetSize)
    );
}
