#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct spank_option {
    pub name: *const ::std::os::raw::c_char,
    pub arginfo: *const ::std::os::raw::c_char,
    pub usage: *const ::std::os::raw::c_char,
    pub has_arg: ::std::os::raw::c_int,
    pub val: ::std::os::raw::c_int,
    pub cb: spank_opt_cb_f,
}

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[cfg(feature = "slurm_20_11")]

mod slurm_error_compat {
    use super::*;

    pub const ESPANK_SUCCESS: u32 = spank_err_ESPANK_SUCCESS;
    pub const slurm_err_t_ESPANK_ERROR: u32 = spank_err_ESPANK_ERROR;
    pub const slurm_err_t_ESPANK_BAD_ARG: u32 = spank_err_ESPANK_BAD_ARG;
    pub const slurm_err_t_ESPANK_NOT_TASK: u32 = spank_err_ESPANK_NOT_TASK;
    pub const slurm_err_t_ESPANK_ENV_EXISTS: u32 = spank_err_ESPANK_ENV_EXISTS;
    pub const slurm_err_t_ESPANK_ENV_NOEXIST: u32 = spank_err_ESPANK_ENV_NOEXIST;
    pub const slurm_err_t_ESPANK_NOSPACE: u32 = spank_err_ESPANK_NOSPACE;
    pub const slurm_err_t_ESPANK_NOT_REMOTE: u32 = spank_err_ESPANK_NOT_REMOTE;
    pub const slurm_err_t_ESPANK_NOEXIST: u32 = spank_err_ESPANK_NOEXIST;
    pub const slurm_err_t_ESPANK_NOT_EXECD: u32 = spank_err_ESPANK_NOT_EXECD;
    pub const slurm_err_t_ESPANK_NOT_AVAIL: u32 = spank_err_ESPANK_NOT_AVAIL;
    pub const slurm_err_t_ESPANK_NOT_LOCAL: u32 = spank_err_ESPANK_NOT_LOCAL;
}

#[cfg(feature = "slurm_20_11")]
pub use slurm_error_compat::*;
