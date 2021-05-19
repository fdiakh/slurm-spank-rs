use slurm_spank::{spank_log, LogLevel, Plugin, SpankHandle, SpankOption, SPANK_PLUGIN};
use std::error::Error;

#[derive(Default)]
struct TestSpank {
    data: u32,
}

impl Plugin for TestSpank {
    fn init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        println!("Plugin arguments: {:?}", spank.plugin_argv());
        println!("Context is {:?}", spank.context()?);
        self.data = 20;
        spank.register_option(SpankOption::new("opt_a").usage("my flag"))?;
        spank.register_option(
            SpankOption::new("opt_b")
                .takes_value("value")
                .usage("my valued option"),
        )?;
        println!("{:?}", spank.job_env());
        Ok(())
    }
    fn task_init(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Error,
            &format!("{:?}", spank.job_supplmentary_gids()?),
        );
        Ok(())
    }
    fn task_exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        spank_log(
            LogLevel::Error,
            &format!("Job env task exit {:?}", spank.job_env()?),
        );

        Ok(())
    }
    fn exit(&mut self, spank: &mut SpankHandle) -> Result<(), Box<dyn Error>> {
        println!("Data is {:?}", self.data);
        println!("CB returned {:?}", spank.get_option_value_os("opt_a"));

        spank_log(LogLevel::Error, "Goodbye from spank logs\n");
        Ok(())
    }
}

SPANK_PLUGIN!(b"atoto\0", 0x130502, TestSpank);
