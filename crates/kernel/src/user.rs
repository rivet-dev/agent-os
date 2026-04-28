#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
    pub supplementary_gids: Vec<u32>,
}

impl Default for ProcessIdentity {
    fn default() -> Self {
        Self {
            uid: 1000,
            gid: 1000,
            euid: 1000,
            egid: 1000,
            supplementary_gids: vec![1000],
        }
    }
}

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
    pub group_name: Option<String>,
    pub supplementary_gids: Vec<u32>,
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
    pub group_name: String,
    pub supplementary_gids: Vec<u32>,
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
        let username = config.username.unwrap_or_else(|| String::from("user"));
        let supplementary_gids = normalize_supplementary_gids(gid, config.supplementary_gids);

        Self {
            uid,
            gid,
            euid: config.euid.unwrap_or(uid),
            egid: config.egid.unwrap_or(gid),
            username: username.clone(),
            homedir: config.homedir.unwrap_or_else(|| String::from("/home/user")),
            shell: config.shell.unwrap_or_else(|| String::from("/bin/sh")),
            gecos: config.gecos.unwrap_or_default(),
            group_name: config.group_name.unwrap_or(username),
            supplementary_gids,
        }
    }

    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            uid: self.uid,
            gid: self.gid,
            euid: self.euid,
            egid: self.egid,
            supplementary_gids: self.supplementary_gids.clone(),
        }
    }

    pub fn getgroups(&self) -> Vec<u32> {
        self.supplementary_gids.clone()
    }

    pub fn getpwuid(&self, uid: u32) -> Option<String> {
        if uid == self.uid {
            return Some(format!(
                "{}:x:{}:{}:{}:{}:{}",
                self.username, self.uid, self.gid, self.gecos, self.homedir, self.shell
            ));
        }

        None
    }

    pub fn getgrgid(&self, gid: u32) -> Option<String> {
        if gid == self.gid {
            return Some(format!(
                "{}:x:{}:{}",
                self.group_name, self.gid, self.username
            ));
        }

        if self.supplementary_gids.contains(&gid) {
            let group_name = format!("group{gid}");
            return Some(format!("{group_name}:x:{gid}:{}", self.username));
        }

        None
    }
}

fn normalize_supplementary_gids(primary_gid: u32, supplementary_gids: Vec<u32>) -> Vec<u32> {
    let mut normalized = Vec::with_capacity(supplementary_gids.len() + 1);
    normalized.push(primary_gid);
    for gid in supplementary_gids {
        if !normalized.contains(&gid) {
            normalized.push(gid);
        }
    }
    normalized
}
