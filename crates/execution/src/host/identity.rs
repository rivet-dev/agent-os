use super::{BoundedString, BoundedUsize, BoundedVec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityIdKind {
    RealUser,
    EffectiveUser,
    SavedUser,
    RealGroup,
    EffectiveGroup,
    SavedGroup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityOperation {
    GetId {
        kind: IdentityIdKind,
    },
    GetUserIds,
    GetGroupIds,
    Get,
    SetId {
        kind: IdentityIdKind,
        value: Option<u32>,
    },
    SetUserIds {
        real: Option<u32>,
        effective: Option<u32>,
        saved: Option<u32>,
    },
    SetRealEffectiveUserIds {
        real: Option<u32>,
        effective: Option<u32>,
    },
    SetGroupIds {
        real: Option<u32>,
        effective: Option<u32>,
        saved: Option<u32>,
    },
    SetRealEffectiveGroupIds {
        real: Option<u32>,
        effective: Option<u32>,
    },
    GetSupplementaryGroups,
    SetSupplementaryGroups {
        groups: BoundedVec<u32>,
    },
    PasswdById {
        uid: u32,
        max_record_bytes: BoundedUsize,
    },
    PasswdByName {
        name: BoundedString,
        max_record_bytes: BoundedUsize,
    },
    NextPasswd {
        index: usize,
        max_record_bytes: BoundedUsize,
    },
    GroupById {
        gid: u32,
        max_record_bytes: BoundedUsize,
    },
    GroupByName {
        name: BoundedString,
        max_record_bytes: BoundedUsize,
    },
    NextGroup {
        index: usize,
        max_record_bytes: BoundedUsize,
    },
}
