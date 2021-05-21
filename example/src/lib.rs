use slurm_spank::{spank_log, Context, LogLevel, Plugin, SpankHandle, SpankOption, SPANK_PLUGIN};
use std::error::Error;

#[derive(Default)]
struct TestSpank {
    data: bool,
}

impl Plugin for TestSpank {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Info,
            &format!(
                "Init from context {:?} with arguments ",
                spank.plugin_argv()?
            ),
        );

        spank.register_option(SpankOption::new("flag").usage("test boolean"))?;
        spank.register_option(
            SpankOption::new("get-var")
                .takes_value("envvar")
                .usage("use this environment variable"),
        )?;

        spank_log(
            LogLevel::Info,
            &format!("Init from context {:?}", spank.context()?),
        );

        Ok(())
    }

    fn init_post_opt(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Info,
            &format!("Init post op from context {:?}", spank.context()?),
        );

        if let Some(v) = spank.get_option_value("get-var")? {
            match spank.context()? {
                Context::Local => println!("envvar {} = {:?}", v, std::env::var(v.to_string())?),
                _ => println!("envvar {} = {:?}", v, spank.getenv(v.to_string())?),
            }
        }

        self.data = spank.is_option_set("flag");

        spank_log(
            LogLevel::Info,
            &format!(
                "Post op context {:?} saving flag {}",
                spank.context()?,
                self.data
            ),
        );

        Ok(())
    }
    fn job_prolog(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Info,
            &format!("Prolog from context {:?}", spank.context()?),
        );
        self.data = spank.is_option_set("flag");
        spank_log(
            LogLevel::Info,
            &format!("Saving flag {} from prolog", self.data),
        );

        Ok(())
    }
    fn user_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Info,
            &format!("Task  user init, saved flag was {}", self.data),
        );
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Error,
            &format!(
                "Saved flags {} Supplementary gids {:?}",
                self.data,
                spank.job_supplmentary_gids()?
            ),
        );
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        println!("Task exit, saved flag was {}", self.data);
        Ok(())
    }
    fn job_epilog(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Info,
            &format!(
                "Epilog from context {:?}, saved flag was {}",
                spank.context()?,
                self.data
            ),
        );
        spank_log(LogLevel::Error, "Goodbye from spank logs\n");
        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Info,
            &format!(
                "Exit from context {:?}, saved flag was {}",
                spank.context()?,
                self.data
            ),
        );
        Ok(())
    }
}

SPANK_PLUGIN!(b"myrsplugin\0", 0x130502, TestSpank);
