#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserConfig {
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub euid: Option<u32>,
    pub egid: Option<u32>,
    pub username: Option<String>,
    pub homedir: Option<String>,
    pub shell: Option<String>,
    pub gecos: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserManager {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
    pub username: String,
    pub homedir: String,
    pub shell: String,
    pub gecos: String,
}

impl Default for UserManager {
    fn default() -> Self {
        Self::from_config(UserConfig::default())
    }
}

impl UserManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_config(config: UserConfig) -> Self {
        let uid = config.uid.unwrap_or(1000);
        let gid = config.gid.unwrap_or(1000);

        Self {
            uid,
            gid,
            euid: config.euid.unwrap_or(uid),
            egid: config.egid.unwrap_or(gid),
            username: config.username.unwrap_or_else(|| String::from("user")),
            homedir: config.homedir.unwrap_or_else(|| String::from("/home/user")),
            shell: config.shell.unwrap_or_else(|| String::from("/bin/sh")),
            gecos: config.gecos.unwrap_or_default(),
        }
    }

    pub fn getpwuid(&self, uid: u32) -> String {
        if uid == self.uid {
            return format!(
                "{}:x:{}:{}:{}:{}:{}",
                self.username, self.uid, self.gid, self.gecos, self.homedir, self.shell
            );
        }

        let username = format!("user{uid}");
        format!("{username}:x:{uid}:{uid}::/home/{username}:/bin/sh")
    }
}
