use eyre::WrapErr;
use slurm_spank::{
    spank_log_user, Context, Plugin, SpankHandle, SpankOption, SLURM_VERSION_NUMBER, SPANK_PLUGIN,
};

use std::error::Error;
use tracing::info;

// All spank plugins must define this macro for the
// Slurm plugin loader.
SPANK_PLUGIN!(b"hello", SLURM_VERSION_NUMBER, SpankHello);

#[derive(Default)]
struct SpankHello {
    greet: Option<String>,
}

unsafe impl Plugin for SpankHello {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        // Register the --greet=name option
        match spank.context()? {
            Context::Local | Context::Remote => {
                spank
                    .register_option(
                        SpankOption::new("greet")
                            .takes_value("name")
                            .usage("Greet [name] before running tasks"),
                    )
                    .wrap_err("Failed to register greet option")?;
            }
            _ => {}
        }
        Ok(())
    }
    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        // Check if the option was set
        self.greet = spank
            .get_option_value("greet")
            .wrap_err("Failed to read --greet option")?
            .map(|s| s.to_string());
        if let Some(name) = &self.greet {
            info!("User opted to greet {name}");
        }
        Ok(())
    }

    fn user_init(&mut self, _spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        // Greet as requested
        if let Some(name) = &self.greet {
            spank_log_user!("Hello {name}!");
        }
        Ok(())
    }
}
