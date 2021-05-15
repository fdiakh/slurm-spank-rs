use byte_strings::{c_str, concat_bytes};
use lazy_static::lazy_static;
use num_enum::{IntoPrimitive, TryFromPrimitive};
//use num::FromPrimitive;
use libc::{gid_t, pid_t, uid_t};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::convert::TryInto;
use std::error::Error;
use std::ffi::{CStr, CString};
use std::fmt;
use std::fmt::Display;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::slice;
use std::sync::Mutex;

mod bindings;

#[repr(C)]
pub struct StaticCStr(*const u8);
unsafe impl Sync for StaticCStr {}

#[derive(Default)]
struct OptionCache {
    options: Vec<String>,
    values: HashMap<String, Vec<Option<String>>>,
}

pub struct SpankHandle<'a> {
    spank: bindings::spank_t,
    opt_cache: &'a mut OptionCache,
    argc: c_int,
    argv: *const *const c_char,
}

macro_rules! spank_item_getter {
    ($name:ident, $spank_item:path, $arg_name:ident, $arg_type:ty, $result_type:ty) => {
        pub fn $name(&self, $arg_name: $arg_type) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe {
                bindings::spank_get_item(self.spank, $spank_item.into(), $arg_name, res_ptr)
            } {
                bindings::spank_err_ESPANK_SUCCESS => Ok(res),
                e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    };
    ($name:ident, $spank_item:path, &str) => {
        pub fn $name(&self) -> Result<&str, SpankError> {
            let mut res: *const c_char = ptr::null_mut();
            let res_ptr: *mut *const c_char = &mut res;
            match unsafe { bindings::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                bindings::spank_err_ESPANK_SUCCESS => {
                    if res.is_null() {
                        Err(SpankError::Generic)
                    } else {
                        unsafe { CStr::from_ptr(res) }
                            .to_str()
                            .map_err(|_| SpankError::Generic)
                    }
                }
                e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    };
    ($name:ident, $spank_item:path,$result_type:ty) => {
        pub fn $name(&self) -> Result<$result_type, SpankError> {
            let mut res: $result_type = <$result_type>::default();
            let res_ptr: *mut $result_type = &mut res;
            match unsafe { bindings::spank_get_item(self.spank, $spank_item.into(), res_ptr) } {
                bindings::spank_err_ESPANK_SUCCESS => Ok(res),
                e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
            }
        }
    };
}

impl<'a> SpankHandle<'a> {
    pub fn context(&self) -> Result<Context, SpankError> {
        let ctx = unsafe { bindings::spank_context() };
        Context::try_from(ctx).map_err(|_| SpankError::Generic)
    }

    pub fn register_option(&mut self, spank_opt: SpankOption) -> Result<(), SpankError> {
        let arginfo = match spank_opt.arginfo {
            None => None,
            Some(info) => Some(CString::new(&info as &str).or(Err(SpankError::BadArg))?),
        };
        let name = CString::new(&spank_opt.name as &str).or(Err(SpankError::BadArg))?;
        let usage = match spank_opt.usage {
            None => None,
            Some(usage) => Some(CString::new(&usage as &str).or(Err(SpankError::BadArg))?),
        };

        let mut c_spank_opt = bindings::spank_option {
            name: name.as_ptr(),
            has_arg: arginfo.is_some() as i32,
            cb: Some(spank_opt_callback),
            arginfo: match arginfo {
                Some(ref arginfo) => arginfo.as_ptr(),
                None => ptr::null(),
            },
            usage: match usage {
                Some(ref usage) => usage.as_ptr(),
                None => ptr::null(),
            },
            val: self
                .opt_cache
                .options
                .len()
                .try_into()
                .or(Err(SpankError::NoSpace))?,
        };

        match unsafe { bindings::spank_option_register(self.spank, &mut c_spank_opt) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                self.opt_cache.options.push(spank_opt.name);
                Ok(())
            }
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn argv(&self) -> Result<Vec<&str>, SpankError> {
        self.argv_to_vec(self.argc as usize, self.argv)
    }

    pub fn job_argv(&self) -> Result<Vec<&str>, SpankError> {
        let mut argc: c_int = 0;
        let mut argv: *const *const c_char = ptr::null_mut();

        let argc_ptr: *mut c_int = &mut argc;
        let argv_ptr: *mut *const *const c_char = &mut argv;

        match unsafe {
            bindings::spank_get_item(self.spank, SpankItem::JobArgv.into(), argc_ptr, argv_ptr)
        } {
            bindings::spank_err_ESPANK_SUCCESS => self.argv_to_vec(argc as usize, argv),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    // TODO: Return a better error type and maybe validate argc earlier
    fn argv_to_vec(
        &self,
        argc: usize,
        argv: *const *const c_char,
    ) -> Result<Vec<&str>, SpankError> {
        unsafe { slice::from_raw_parts(argv, argc) }
            .iter()
            .map(|&arg| unsafe { CStr::from_ptr(arg) }.to_str())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| SpankError::Generic)
    }

    pub fn job_supplmentary_gids(&self) -> Result<Vec<gid_t>, SpankError> {
        let mut gidc: c_int = 0;
        let mut gidv: *const gid_t = ptr::null_mut();

        let gidc_ptr: *mut c_int = &mut gidc;
        let gidv_ptr: *mut *const gid_t = &mut gidv;

        match unsafe {
            bindings::spank_get_item(self.spank, SpankItem::JobEnv.into(), gidv_ptr, gidc_ptr)
        } {
            bindings::spank_err_ESPANK_SUCCESS => {
                Ok(unsafe { slice::from_raw_parts(gidv, gidc as usize) }
                    .iter()
                    .map(|&gid| gid)
                    .collect::<Vec<gid_t>>())
            }
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn job_env(&self) -> Result<Vec<&str>, SpankError> {
        let mut argv: *const *const c_char = ptr::null_mut();
        let argv_ptr: *mut *const *const c_char = &mut argv;

        match unsafe { bindings::spank_get_item(self.spank, SpankItem::JobEnv.into(), argv_ptr) } {
            bindings::spank_err_ESPANK_SUCCESS => {
                if argv.is_null() {
                    return Err(SpankError::Generic);
                }
                let mut argc: isize = 0;
                while !unsafe { *argv.offset(argc as isize) }.is_null() {
                    argc += 1;
                }
                self.argv_to_vec(argc as usize, argv)
            }
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn setenv(&self, name: &str, value: &str, overwrite: bool) -> Result<(), SpankError> {
        let c_name = CString::new(name).map_err(|_| SpankError::BadArg)?;
        let c_value = CString::new(value).map_err(|_| SpankError::BadArg)?;

        match unsafe {
            bindings::spank_setenv(
                self.spank,
                c_name.as_ptr(),
                c_value.as_ptr(),
                overwrite as c_int,
            )
        } {
            bindings::spank_err_ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn unsetenv(&self, name: &str) -> Result<(), SpankError> {
        let c_name = CString::new(name).map_err(|_| SpankError::BadArg)?;
        match unsafe { bindings::spank_unsetenv(self.spank, c_name.as_ptr()) } {
            bindings::spank_err_ESPANK_SUCCESS => Ok(()),
            e => Err(SpankError::try_from(e).unwrap_or(SpankError::Generic)),
        }
    }

    pub fn get_option_value(&self, name: &str) -> Option<&str> {
        if let Some(values) = self.opt_cache.values.get(name) {
            if let Some(value) = values.get(0) {
                value.as_deref()
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn get_option_values(&self, name: &str) -> Option<Vec<&str>> {
        if let Some(values) = self.opt_cache.values.get(name) {
            values
                .iter()
                .map(|o| o.as_deref().ok_or(()))
                .collect::<Result<Vec<&str>, ()>>()
                .ok()
        } else {
            None
        }
    }

    pub fn get_option_count(&self, name: &str) -> usize {
        if let Some(values) = self.opt_cache.values.get(name) {
            values.len()
        } else {
            0
        }
    }

    spank_item_getter!(job_gid, SpankItem::JobGid, gid_t);
    spank_item_getter!(job_uid, SpankItem::JobUid, uid_t);
    spank_item_getter!(job_id, SpankItem::JobId, u32);
    spank_item_getter!(job_stepid, SpankItem::JobStepid, u32);
    spank_item_getter!(job_nnodes, SpankItem::JobNnodes, u32);
    spank_item_getter!(job_nodeid, SpankItem::JobNodeid, u32);
    spank_item_getter!(job_local_task_count, SpankItem::JobLocalTaskCount, u32);
    spank_item_getter!(job_total_task_count, SpankItem::JobTotalTaskCount, u32);
    spank_item_getter!(job_ncpus, SpankItem::JobNcpus, u16);
    spank_item_getter!(task_id, SpankItem::TaskId, c_int);
    spank_item_getter!(task_global_id, SpankItem::TaskGlobalId, u32);
    spank_item_getter!(task_exit_status, SpankItem::TaskExitStatus, c_int);
    spank_item_getter!(task_pid, SpankItem::TaskPid, pid_t);
    spank_item_getter!(
        pid_to_global_id,
        SpankItem::JobPidToGlobalId,
        pid,
        pid_t,
        u32
    );
    spank_item_getter!(pid_to_local_id, SpankItem::JobPidToLocalId, pid, pid_t, u32);
    spank_item_getter!(
        local_to_global_id,
        SpankItem::JobLocalToGlobalId,
        local_id,
        u32,
        u32
    );
    spank_item_getter!(
        global_to_local_id,
        SpankItem::JobGlobalToLocalId,
        global_id,
        u32,
        u32
    );
    spank_item_getter!(job_alloc_mem, SpankItem::JobAllocMem, u64);
    spank_item_getter!(slurm_restart_count, SpankItem::SlurmRestartCount, u32);
    spank_item_getter!(job_array_id, SpankItem::JobArrayId, u32);
    spank_item_getter!(job_array_task_id, SpankItem::JobArrayTaskId, u32);
    spank_item_getter!(slurm_version, SpankItem::SlurmVersion, &str);
    spank_item_getter!(slurm_version_major, SpankItem::SlurmVersionMajor, &str);
    spank_item_getter!(slurm_version_minor, SpankItem::SlurmVersionMinor, &str);
    spank_item_getter!(slurm_version_micro, SpankItem::SlurmVersionMicro, &str);
}

fn cstring_escape_null(msg: &str) -> CString {
    // XXX: We can't deal with NULL characters here, but how do we expect a
    // plugin author to handle the error if we returned one ? We assume they
    // would prefer that we render them as a 0 in the logs instead.
    let c_safe_msg = msg.split('\0').collect::<Vec<&str>>().join("0");

    // Should never panic as we made sure there is no NULL chars
    CString::new(&c_safe_msg as &str).unwrap()
}

pub enum LogLevel {
    Error,
    Info,
    Verbose,
    Debug,
    Debug2,
    Debug3,
}

pub fn spank_log(level: LogLevel, msg: &str) {
    let c_msg = cstring_escape_null(msg);

    match level {
        LogLevel::Error => unsafe { bindings::slurm_error(c_str!("%s").as_ptr(), c_msg.as_ptr()) },
        LogLevel::Info => unsafe { bindings::slurm_info(c_str!("%s").as_ptr(), c_msg.as_ptr()) },
        LogLevel::Verbose => unsafe {
            bindings::slurm_verbose(c_str!("%s").as_ptr(), c_msg.as_ptr())
        },
        LogLevel::Debug => unsafe { bindings::slurm_debug(c_str!("%s").as_ptr(), c_msg.as_ptr()) },
        LogLevel::Debug2 => unsafe {
            bindings::slurm_debug2(c_str!("%s").as_ptr(), c_msg.as_ptr())
        },
        LogLevel::Debug3 => unsafe {
            bindings::slurm_debug3(c_str!("%s").as_ptr(), c_msg.as_ptr())
        },
    }
}

#[no_mangle]
pub extern "C" fn spank_opt_callback(val: c_int, optarg: *const c_char, _remote: c_int) -> c_int {
    let mut opt_cache = match OPTIONS.lock() {
        Ok(opt_cache) => opt_cache,
        // TODO: Log err
        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
    };

    let name = match opt_cache.options.get(val as usize) {
        Some(name) => name.clone(),
        // TODO: Log err
        _ => return bindings::spank_err_ESPANK_ERROR as c_int,
    };

    let optarg = if optarg == ptr::null() {
        None
    } else {
        let cstr = unsafe { CStr::from_ptr(optarg) };
        match cstr.to_str() {
            Ok(cstr) => Some(cstr.to_string()),
            // TODO: Log verb
            _ => return bindings::spank_err_ESPANK_ERROR as c_int,
        }
    };

    opt_cache.values.entry(name).or_insert(vec![]).push(optarg);

    bindings::spank_err_ESPANK_SUCCESS as c_int
}

lazy_static! {
    // XXX Slurm should never call us from multiple threads so we
    // could probably use a RefCell or UnsafeCell and force it Sync
    static ref OPTIONS: Mutex<OptionCache> = Mutex::new(OptionCache::default());
}

#[macro_export]
macro_rules! SPANK_PLUGIN {
    ($spank_name:literal, $spank_version:literal, $spank_ty:ty) => {
        #[no_mangle]
        pub static plugin_name: StaticCStr =
            StaticCStr({ concat_bytes!($spank_name, b"\0") } as *const u8);
        #[no_mangle]
        pub static plugin_type: StaticCStr = StaticCStr(b"spank" as *const u8);
        // #[no_mangle]
        // pub static plugin_version: c_int = $spank_version;

        lazy_static! {
            // XXX Slurm should never call us from multiple threads so we
            // could probably use a RefCell or UnsafeCell and force it Sync
            static ref PLUGIN: Mutex<$spank_ty> = Mutex::new(<$spank_ty>::default());

        }

        fn _check_spank_trait<T: Plugin>() {}
        fn _t() {
            _check_spank_trait::<$spank_ty>()
        }

        macro_rules! spank_hook {
            ($c_spank_cb:ident, $rust_spank_cb:ident) => {
                #[no_mangle]
                #[doc(hidden)]
                pub extern "C" fn $c_spank_cb(
                    spank: bindings::spank_t,
                    ac: c_int,
                    argv: *const *const c_char,
                ) -> c_int {
                    let mut plugin = match PLUGIN.lock() {
                        Ok(plugin) => plugin,
                        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
                    };

                    let mut opt_cache = match OPTIONS.lock() {
                        Ok(cache) => cache,
                        Err(_) => return bindings::spank_err_ESPANK_ERROR as c_int,
                    };

                    match plugin.$rust_spank_cb(&mut SpankHandle {
                        spank: spank,
                        opt_cache: &mut opt_cache,
                        argc: ac,
                        argv: argv,
                    }) {
                        Ok(()) => bindings::spank_err_ESPANK_SUCCESS as c_int,
                        Err(err) => -(err as c_int),
                    }
                }
            };
        }

        spank_hook!(slurm_spank_init, init);
        spank_hook!(slurm_spank_job_prolog, job_prolog);
        spank_hook!(slurm_spank_init_post_opt, init_post_opt);
        spank_hook!(slurm_spank_local_user_init, local_user_init);
        spank_hook!(slurm_spank_user_init, user_init);
        spank_hook!(slurm_spank_task_init_privileged, task_init_privileged);
        spank_hook!(slurm_spank_task_init, task_init);
        spank_hook!(slurm_spank_task_post_fork, task_post_fork);
        spank_hook!(slurm_spank_task_exit, task_exit);
        spank_hook!(slurm_spank_job_epilog, job_epilog);
        spank_hook!(slurm_spank_slurmd_exit, slurmd_exit);
        spank_hook!(slurm_spank_exit, exit);
    };
}

#[allow(unused_variables)]
pub trait Plugin {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn job_prolog(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn local_user_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn user_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_init_privileged(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_post_fork(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn job_epilog(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn slurmd_exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        Ok(())
    }
}
impl Error for SpankError {}

impl Display for SpankError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cerr = unsafe { CStr::from_ptr(bindings::spank_strerror(*self as u32)) };

        if let Ok(err) = cerr.to_str() {
            write!(f, "{}", err)
        } else {
            write!(f, "Invalid Error")
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
pub enum SpankItem {
    JobGid = bindings::spank_item_S_JOB_GID,
    JobUid = bindings::spank_item_S_JOB_UID,
    JobId = bindings::spank_item_S_JOB_ID,
    JobStepid = bindings::spank_item_S_JOB_STEPID,
    JobNnodes = bindings::spank_item_S_JOB_NNODES,
    JobNodeid = bindings::spank_item_S_JOB_NODEID,
    JobLocalTaskCount = bindings::spank_item_S_JOB_LOCAL_TASK_COUNT,
    JobTotalTaskCount = bindings::spank_item_S_JOB_TOTAL_TASK_COUNT,
    JobNcpus = bindings::spank_item_S_JOB_NCPUS,
    JobArgv = bindings::spank_item_S_JOB_ARGV,
    JobEnv = bindings::spank_item_S_JOB_ENV,
    TaskId = bindings::spank_item_S_TASK_ID,
    TaskGlobalId = bindings::spank_item_S_TASK_GLOBAL_ID,
    TaskExitStatus = bindings::spank_item_S_TASK_EXIT_STATUS,
    TaskPid = bindings::spank_item_S_TASK_PID,
    JobPidToGlobalId = bindings::spank_item_S_JOB_PID_TO_GLOBAL_ID,
    JobPidToLocalId = bindings::spank_item_S_JOB_PID_TO_LOCAL_ID,
    JobLocalToGlobalId = bindings::spank_item_S_JOB_LOCAL_TO_GLOBAL_ID,
    JobGlobalToLocalId = bindings::spank_item_S_JOB_GLOBAL_TO_LOCAL_ID,
    JobSupplementaryGids = bindings::spank_item_S_JOB_SUPPLEMENTARY_GIDS,
    SlurmVersion = bindings::spank_item_S_SLURM_VERSION,
    SlurmVersionMajor = bindings::spank_item_S_SLURM_VERSION_MAJOR,
    SlurmVersionMinor = bindings::spank_item_S_SLURM_VERSION_MINOR,
    SlurmVersionMicro = bindings::spank_item_S_SLURM_VERSION_MICRO,
    StepCpusPerTask = bindings::spank_item_S_STEP_CPUS_PER_TASK,
    JobAllocCores = bindings::spank_item_S_JOB_ALLOC_CORES,
    JobAllocMem = bindings::spank_item_S_JOB_ALLOC_MEM,
    StepAllocCores = bindings::spank_item_S_STEP_ALLOC_CORES,
    StepAllocMem = bindings::spank_item_S_STEP_ALLOC_MEM,
    SlurmRestartCount = bindings::spank_item_S_SLURM_RESTART_COUNT,
    JobArrayId = bindings::spank_item_S_JOB_ARRAY_ID,
    JobArrayTaskId = bindings::spank_item_S_JOB_ARRAY_TASK_ID,
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
pub enum SpankError {
    Generic = bindings::spank_err_ESPANK_ERROR,
    BadArg = bindings::spank_err_ESPANK_BAD_ARG,
    NotTask = bindings::spank_err_ESPANK_NOT_TASK,
    EnvExists = bindings::spank_err_ESPANK_ENV_EXISTS,
    EnvNotExist = bindings::spank_err_ESPANK_ENV_NOEXIST,
    NoSpace = bindings::spank_err_ESPANK_NOSPACE,
    NotRemote = bindings::spank_err_ESPANK_NOT_REMOTE,
    NoExist = bindings::spank_err_ESPANK_NOEXIST,
    NotExecd = bindings::spank_err_ESPANK_NOT_EXECD,
    NotAvail = bindings::spank_err_ESPANK_NOT_AVAIL,
    NotLocal = bindings::spank_err_ESPANK_NOT_LOCAL,
}

#[derive(Debug, Copy, Clone, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u32)] // FIXME: force the type generated by bindgen
pub enum Context {
    // We dont represent error here, as errors are better embedded in Results
    Local = bindings::spank_context_S_CTX_LOCAL,
    Remote = bindings::spank_context_S_CTX_REMOTE,
    Allocator = bindings::spank_context_S_CTX_ALLOCATOR,
    Slurmd = bindings::spank_context_S_CTX_SLURMD,
    JobScript = bindings::spank_context_S_CTX_JOB_SCRIPT,
}

pub struct SpankOption {
    name: String,
    arginfo: Option<String>,
    usage: Option<String>,
}

impl SpankOption {
    pub fn new(name: &str) -> Self {
        SpankOption {
            name: name.to_string(),
            arginfo: None,
            usage: None,
        }
    }
    pub fn usage(mut self, usage: &str) -> Self {
        self.usage = Some(usage.to_string());
        self
    }
    pub fn has_arg(mut self, arg_name: &str) -> Self {
        self.arginfo = Some(arg_name.to_string());
        self
    }
}

// Example below
#[derive(Default)]
struct TestSpank {
    data: u32,
}

impl Plugin for TestSpank {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        println!(
            "C'est l'initialisation et les arguments sont: {:?}",
            spank.argv()
        );
        println!("Context is {:?}", spank.context()?);
        self.data = 20;
        spank.register_option(SpankOption::new("opt_a").usage("c'est une option"))?;
        // Ok(vec![
        //     SpankOption::new("opt_a"),
        //     SpankOption::new("opt_b").has_arg("val"),
        // ])
        Ok(())
    }

    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), SpankError> {
        println!("Data is {:?}", self.data);
        println!("CB returned {:?}", spank.get_option_value("opt_a"));

        spank_log(LogLevel::Error, "Goodbye from spank logs\n");
        Ok(())
    }
}

SPANK_PLUGIN!(b"toto", 0x130502, TestSpank);
