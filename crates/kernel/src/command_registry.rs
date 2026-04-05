use crate::vfs::{VfsResult, VirtualFileSystem};
use std::collections::BTreeMap;

const COMMAND_STUB: &[u8] = b"#!/bin/sh\n# kernel command stub\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDriver {
    name: String,
    commands: Vec<String>,
}

impl CommandDriver {
    pub fn new<N, I, C>(name: N, commands: I) -> Self
    where
        N: Into<String>,
        I: IntoIterator<Item = C>,
        C: Into<String>,
    {
        Self {
            name: name.into(),
            commands: commands.into_iter().map(Into::into).collect(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn commands(&self) -> &[String] {
        &self.commands
    }
}

#[derive(Debug, Default, Clone)]
pub struct CommandRegistry {
    commands: BTreeMap<String, CommandDriver>,
    warnings: Vec<String>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, driver: CommandDriver) {
        for command in &driver.commands {
            if let Some(existing) = self.commands.get(command) {
                self.warnings.push(format!(
                    "command \"{command}\" overridden: {} -> {}",
                    existing.name(),
                    driver.name()
                ));
            }

            self.commands.insert(command.clone(), driver.clone());
        }
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn resolve(&self, command: &str) -> Option<&CommandDriver> {
        self.commands.get(command)
    }

    pub fn list(&self) -> BTreeMap<String, String> {
        self.commands
            .iter()
            .map(|(command, driver)| (command.clone(), driver.name().to_owned()))
            .collect()
    }

    pub fn populate_bin<F>(&self, vfs: &mut F) -> VfsResult<()>
    where
        F: VirtualFileSystem,
    {
        if !vfs.exists("/bin") {
            vfs.mkdir("/bin", true)?;
        }

        for command in self.commands.keys() {
            let path = format!("/bin/{command}");
            if !vfs.exists(&path) {
                vfs.write_file(&path, COMMAND_STUB.to_vec())?;
                let _ = vfs.chmod(&path, 0o755);
            }
        }

        Ok(())
    }
}
