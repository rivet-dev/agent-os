use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub uid: u32,
    pub gid: u32,
    pub euid: u32,
    pub egid: u32,
    pub suid: u32,
    pub sgid: u32,
    pub supplementary_gids: Vec<u32>,
}

impl Default for ProcessIdentity {
    fn default() -> Self {
        Self {
            uid: 1000,
            gid: 1000,
            euid: 1000,
            egid: 1000,
            suid: 1000,
            sgid: 1000,
            supplementary_gids: vec![1000],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAccount {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
    pub homedir: String,
    pub shell: String,
    pub gecos: String,
    pub supplementary_gids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRecord {
    pub gid: u32,
    pub name: String,
    pub members: Vec<String>,
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
    /// Supplementary groups are VM configuration, not guest-mutable state.
    /// The primary gid is always injected and duplicate gids are dropped.
    pub supplementary_gids: Vec<u32>,
    pub accounts: Vec<UserAccount>,
    pub groups: Vec<GroupRecord>,
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
    accounts_by_uid: BTreeMap<u32, UserAccount>,
    account_uids_by_name: BTreeMap<String, u32>,
    groups_by_gid: BTreeMap<u32, GroupRecord>,
    group_gids_by_name: BTreeMap<String, u32>,
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
        let username = config.username.unwrap_or_else(|| String::from("agentos"));
        let supplementary_gids = normalize_supplementary_gids(gid, config.supplementary_gids);

        let primary_account = UserAccount {
            uid,
            gid,
            username: username.clone(),
            homedir: config
                .homedir
                .clone()
                .unwrap_or_else(|| String::from("/home/agentos")),
            shell: config
                .shell
                .clone()
                .unwrap_or_else(|| String::from("/bin/sh")),
            gecos: config.gecos.clone().unwrap_or_default(),
            supplementary_gids: supplementary_gids.clone(),
        };
        let mut accounts_by_uid = BTreeMap::new();
        let mut account_uids_by_name = BTreeMap::new();
        for mut account in config.accounts {
            account.supplementary_gids =
                normalize_supplementary_gids(account.gid, account.supplementary_gids);
            account_uids_by_name.insert(account.username.clone(), account.uid);
            accounts_by_uid.insert(account.uid, account);
        }
        account_uids_by_name.insert(primary_account.username.clone(), primary_account.uid);
        accounts_by_uid.insert(primary_account.uid, primary_account.clone());

        let primary_group_name = config.group_name.unwrap_or_else(|| username.clone());
        let mut groups_by_gid = BTreeMap::new();
        for group in config.groups {
            groups_by_gid.insert(group.gid, group);
        }
        groups_by_gid.entry(gid).or_insert_with(|| GroupRecord {
            gid,
            name: primary_group_name.clone(),
            members: vec![username.clone()],
        });
        let mut synthesized_members = BTreeMap::<u32, Vec<String>>::new();
        for account in accounts_by_uid.values() {
            for account_gid in &account.supplementary_gids {
                if groups_by_gid.contains_key(account_gid) {
                    continue;
                }
                let members = synthesized_members.entry(*account_gid).or_default();
                if !members.contains(&account.username) {
                    members.push(account.username.clone());
                }
            }
        }
        for (group_gid, members) in synthesized_members {
            groups_by_gid.insert(
                group_gid,
                GroupRecord {
                    gid: group_gid,
                    name: format!("group{group_gid}"),
                    members,
                },
            );
        }
        let group_gids_by_name = groups_by_gid
            .values()
            .map(|group| (group.name.clone(), group.gid))
            .collect();

        Self {
            uid,
            gid,
            euid: config.euid.unwrap_or(uid),
            egid: config.egid.unwrap_or(gid),
            username: username.clone(),
            homedir: primary_account.homedir,
            shell: primary_account.shell,
            gecos: primary_account.gecos,
            group_name: primary_group_name,
            supplementary_gids,
            accounts_by_uid,
            account_uids_by_name,
            groups_by_gid,
            group_gids_by_name,
        }
    }

    pub fn identity(&self) -> ProcessIdentity {
        ProcessIdentity {
            uid: self.uid,
            gid: self.gid,
            euid: self.euid,
            egid: self.egid,
            suid: self.euid,
            sgid: self.egid,
            supplementary_gids: self.supplementary_gids.clone(),
        }
    }

    pub fn getgroups(&self) -> Vec<u32> {
        self.supplementary_gids.clone()
    }

    pub fn getpwuid(&self, uid: u32) -> Option<String> {
        self.accounts_by_uid.get(&uid).map(render_passwd)
    }

    pub fn getpwnam(&self, username: &str) -> Option<String> {
        self.account_uids_by_name
            .get(username)
            .and_then(|uid| self.getpwuid(*uid))
    }

    pub fn getgrgid(&self, gid: u32) -> Option<String> {
        self.groups_by_gid.get(&gid).map(render_group)
    }

    pub fn getgrnam(&self, name: &str) -> Option<String> {
        self.group_gids_by_name
            .get(name)
            .and_then(|gid| self.getgrgid(*gid))
    }

    pub fn passwd_entries(&self) -> Vec<String> {
        self.accounts_by_uid.values().map(render_passwd).collect()
    }

    pub fn group_entries(&self) -> Vec<String> {
        self.groups_by_gid.values().map(render_group).collect()
    }

    pub fn account(&self, uid: u32) -> Option<&UserAccount> {
        self.accounts_by_uid.get(&uid)
    }
}

fn render_passwd(account: &UserAccount) -> String {
    format!(
        "{}:x:{}:{}:{}:{}:{}",
        account.username, account.uid, account.gid, account.gecos, account.homedir, account.shell
    )
}

fn render_group(group: &GroupRecord) -> String {
    format!("{}:x:{}:{}", group.name, group.gid, group.members.join(","))
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

pub(crate) fn passwd_record_by_uid(database: &[u8], uid: u32) -> Option<String> {
    passwd_records(database)
        .find(|record| record.uid == uid)
        .map(|record| record.text.to_owned())
}

pub(crate) fn passwd_record_by_name(database: &[u8], name: &str) -> Option<String> {
    passwd_records(database)
        .find(|record| record.name == name)
        .map(|record| record.text.to_owned())
}

pub(crate) fn passwd_record_at(database: &[u8], index: usize) -> Option<String> {
    passwd_records(database)
        .nth(index)
        .map(|record| record.text.to_owned())
}

pub(crate) fn group_record_by_gid(database: &[u8], gid: u32) -> Option<String> {
    group_records(database)
        .find(|record| record.gid == gid)
        .map(|record| record.text.to_owned())
}

pub(crate) fn group_record_by_name(database: &[u8], name: &str) -> Option<String> {
    group_records(database)
        .find(|record| record.name == name)
        .map(|record| record.text.to_owned())
}

pub(crate) fn group_record_at(database: &[u8], index: usize) -> Option<String> {
    group_records(database)
        .nth(index)
        .map(|record| record.text.to_owned())
}

#[derive(Debug, Clone, Copy)]
struct PasswdDatabaseRecord<'a> {
    text: &'a str,
    name: &'a str,
    uid: u32,
}

fn passwd_records(database: &[u8]) -> impl Iterator<Item = PasswdDatabaseRecord<'_>> {
    database.split(|byte| *byte == b'\n').filter_map(|line| {
        if line.contains(&b'\0') {
            return None;
        }
        let text = std::str::from_utf8(line).ok()?;
        let mut fields = text.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let uid = fields.next()?.parse().ok()?;
        let _gid = fields.next()?.parse::<u32>().ok()?;
        let _gecos = fields.next()?;
        let _home = fields.next()?;
        let _shell = fields.next()?;
        fields
            .next()
            .is_none()
            .then_some(PasswdDatabaseRecord { text, name, uid })
    })
}

#[derive(Debug, Clone, Copy)]
struct GroupDatabaseRecord<'a> {
    text: &'a str,
    name: &'a str,
    gid: u32,
}

fn group_records(database: &[u8]) -> impl Iterator<Item = GroupDatabaseRecord<'_>> {
    database.split(|byte| *byte == b'\n').filter_map(|line| {
        if line.contains(&b'\0') {
            return None;
        }
        let text = std::str::from_utf8(line).ok()?;
        let mut fields = text.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let gid = fields.next()?.parse().ok()?;
        let _members = fields.next()?;
        fields
            .next()
            .is_none()
            .then_some(GroupDatabaseRecord { text, name, gid })
    })
}

#[cfg(test)]
mod database_tests {
    use super::*;

    #[test]
    fn account_database_parsers_skip_malformed_records_and_preserve_text() {
        let passwd = b"\n# comment\nbad\nnul\0suffix:x:3:3::/:/bin/sh\ninvalid-utf8:x:\xff:2::/:/bin/sh\nextra:x:1:2::/:/bin/sh:field\nmissing:x:1:2::/bin/sh\noverflow:x:4294967296:2::/:/bin/sh\nroot:x:0:0:root:/root:/bin/sh\nroot:x:9:9:duplicate:/duplicate:/bin/false\nalias:x:0:0:duplicate:/duplicate:/bin/false\ncr:x:7:7::/:/bin/sh\r\n";
        assert_eq!(
            passwd_record_by_name(passwd, "root").as_deref(),
            Some("root:x:0:0:root:/root:/bin/sh")
        );
        assert_eq!(passwd_record_by_uid(passwd, 0), passwd_record_at(passwd, 0));
        assert_eq!(
            passwd_record_at(passwd, 1).as_deref(),
            Some("root:x:9:9:duplicate:/duplicate:/bin/false")
        );
        assert_eq!(
            passwd_record_at(passwd, 2).as_deref(),
            Some("alias:x:0:0:duplicate:/duplicate:/bin/false")
        );
        assert_eq!(
            passwd_record_at(passwd, 3).as_deref(),
            Some("cr:x:7:7::/:/bin/sh\r")
        );
        assert_eq!(passwd_record_at(passwd, 4), None);

        let group = b"\n# comment\nbad\nnul\0suffix:x:3:user\ninvalid-utf8:x:\xff:user\nextra:x:1:user:field\nmissing:x:1\noverflow:x:4294967296:user\nroot:x:0:root\nroot:x:9:duplicate\nalias:x:0:duplicate\ncr:x:7:user\r\n";
        assert_eq!(
            group_record_by_name(group, "root").as_deref(),
            Some("root:x:0:root")
        );
        assert_eq!(group_record_by_gid(group, 0), group_record_at(group, 0));
        assert_eq!(
            group_record_at(group, 1).as_deref(),
            Some("root:x:9:duplicate")
        );
        assert_eq!(
            group_record_at(group, 2).as_deref(),
            Some("alias:x:0:duplicate")
        );
        assert_eq!(group_record_at(group, 3).as_deref(), Some("cr:x:7:user\r"));
        assert_eq!(group_record_at(group, 4), None);
    }
}
