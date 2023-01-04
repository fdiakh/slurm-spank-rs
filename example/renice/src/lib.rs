use eyre::{eyre, Report, WrapErr};
use libc::{setpriority, PRIO_PROCESS};
use slurm_spank::{Context, Plugin, SpankHandle, SpankOption, SPANK_PLUGIN};
use std::error::Error;
use tracing::{error, info};

//  Minimum allowable value for priority. May bex
//  set globally via plugin option min_prio=<prio>
const MIN_PRIO: i32 = -20;
const PRIO_ENV_VAR: &str = "SLURM_RENICE";

// All spank plugins must define this macro for the
// Slurm plugin loader.
SPANK_PLUGIN!(b"renice", 0x160502, SpankRenice);

struct SpankRenice {
    min_prio: i32,
    prio: Option<i32>,
}

// A default instance of the plugin is created when it
// is loaded by Slurm
impl Default for SpankRenice {
    fn default() -> Self {
        Self {
            // Minimum allowable value for priority. May be
            // set globally via plugin option min_prio=<prio>
            min_prio: MIN_PRIO,
            prio: None,
        }
    }
}

unsafe impl Plugin for SpankRenice {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        if spank.context()? == Context::Slurmd {
            error!("Plugin init: {l}", l = spank.plugin_argv()?.len());
        }
        // Don't do anything in slurmd/sbatch/salloc
        if spank.context()? == Context::Allocator {
            return Ok(());
        }
        if spank.context()? == Context::Remote {
            // Parse plugin configuration file
            for arg in spank.plugin_argv().wrap_err("Invalid plugin argument")? {
                match arg.strip_prefix("min_prio=") {
                    Some(value) => {
                        self.min_prio = parse_prio(value).wrap_err("Invalid min_prio")?
                    }
                    None => return Err(eyre!("Invalid plugin argument: {}", arg).into()),
                }
            }
        }
        // Provide a --renice=prio option to srun
        spank
            .register_option(
                SpankOption::new("renice")
                    .takes_value("prio")
                    .usage("Re-nice job tasks to priority [prio]"),
            )
            .wrap_err("Failed to register renice option")?;

        Ok(())
    }
    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        // Skip argument processing outside of relevent contexts
        match spank.context()? {
            Context::Local | Context::Remote => (),
            _ => return Ok(()),
        }

        let prio = spank
            .get_option_value("renice")
            .wrap_err("Failed to read --renice option")?;

        let prio = match prio {
            None => {
                return Ok(());
            }
            Some(prio) => prio,
        };

        self.set_prio(&prio, "--renice")
            .wrap_err("Bad value for --renice")?;

        Ok(())
    }

    fn task_post_fork(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        if self.prio.is_none() {
            // See if SLURM_RENICE env var is set by user
            if let Some(prio) = spank
                .getenv(PRIO_ENV_VAR)
                .wrap_err(format!("Bad value for {}", PRIO_ENV_VAR))?
            {
                self.set_prio(&prio, PRIO_ENV_VAR)
                    .wrap_err_with(|| format!("Bad value for {}", PRIO_ENV_VAR))?;
            }
        }

        if let Some(prio) = self.prio {
            let task_id = spank.task_global_id()?;
            let pid = spank.task_pid()?;

            info!("re-nicing task{} pid {} to {}", task_id, pid, prio);
            if unsafe { setpriority(PRIO_PROCESS, pid as u32, prio) } < 0 {
                return Err(Report::new(std::io::Error::last_os_error())
                    .wrap_err("setpriority")
                    .into());
            }
        }
        Ok(())
    }
}

impl SpankRenice {
    fn set_prio(&mut self, prio: &str, opt_name: &str) -> Result<(), Report> {
        let prio = parse_prio(prio)?;

        self.prio = if prio >= self.min_prio {
            Some(prio)
        } else {
            error!(
                "{}={} is not allowed, will use min_prio ({})",
                opt_name, prio, self.min_prio
            );
            Some(self.min_prio)
        };

        Ok(())
    }
}

fn parse_prio(value: &str) -> Result<i32, Report> {
    let value: i32 = value.parse()?;
    match value {
        -20..=19 => Ok(value),
        _ => Err(eyre!("Priority is not between -20 and 19")),
    }
}
