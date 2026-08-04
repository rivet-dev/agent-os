use crate::vfs::{VfsError, VfsResult, VirtualFileSystem};
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

    fn validate_commands(&self) -> VfsResult<()> {
        for command in &self.commands {
            validate_command_name(command)?;
        }

        Ok(())
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

    pub fn register(&mut self, driver: CommandDriver) -> VfsResult<()> {
        driver.validate_commands()?;

        // Registering one logical driver is replacement, not an append. This is
        // required for transactional registries (for example host callbacks):
        // rolling a driver back to its previous command set must make aliases
        // introduced by the failed update unresolvable.
        self.commands
            .retain(|_, existing| existing.name() != driver.name());

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

        Ok(())
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
        self.populate_commands(vfs, self.commands.keys())
    }

    pub fn populate_driver_bin<F>(&self, vfs: &mut F, driver: &CommandDriver) -> VfsResult<()>
    where
        F: VirtualFileSystem,
    {
        self.populate_commands(vfs, driver.commands())
    }

    fn populate_commands<F, I, S>(&self, vfs: &mut F, commands: I) -> VfsResult<()>
    where
        F: VirtualFileSystem,
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let commands = commands
            .into_iter()
            .map(|command| {
                validate_command_name(command.as_ref())?;
                Ok(command.as_ref().to_owned())
            })
            .collect::<VfsResult<Vec<_>>>()?;

        if !vfs.exists("/bin") {
            vfs.mkdir("/bin", true)?;
        }

        for command in commands {
            let path = format!("/bin/{command}");
            if !vfs.exists(&path) {
                vfs.write_file(&path, COMMAND_STUB.to_vec())?;
                let _ = vfs.chmod(&path, 0o755);
            }
        }

        Ok(())
    }
}

fn validate_command_name(command: &str) -> VfsResult<()> {
    if command.is_empty()
        || command == "."
        || command == ".."
        || command.contains('/')
        || command.contains('\0')
    {
        return Err(VfsError::new(
            "EINVAL",
            format!("invalid command name {command:?}"),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_same_driver_replaces_its_command_set() {
        let mut registry = CommandRegistry::new();
        registry
            .register(CommandDriver::new("bindings", ["old", "temporary"]))
            .expect("register initial driver");
        registry
            .register(CommandDriver::new("bindings", ["old"]))
            .expect("replace driver commands");

        assert_eq!(
            registry.resolve("old").map(CommandDriver::name),
            Some("bindings")
        );
        assert!(
            registry.resolve("temporary").is_none(),
            "aliases removed by a driver refresh must not remain executable"
        );
    }
}
