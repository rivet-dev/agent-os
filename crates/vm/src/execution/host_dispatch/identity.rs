use super::*;

pub(super) struct IdentityCapability;

impl SidecarHostCapability<IdentityOperation> for IdentityCapability {
    fn requires_claim(operation: &IdentityOperation) -> bool {
        !matches!(
            operation,
            IdentityOperation::GetId { .. }
                | IdentityOperation::GetUserIds
                | IdentityOperation::GetGroupIds
                | IdentityOperation::Get
                | IdentityOperation::GetSupplementaryGroups
                | IdentityOperation::PasswdById { .. }
                | IdentityOperation::PasswdByName { .. }
                | IdentityOperation::NextPasswd { .. }
                | IdentityOperation::GroupById { .. }
                | IdentityOperation::GroupByName { .. }
                | IdentityOperation::NextGroup { .. }
        )
    }

    fn execute(
        kernel: &mut SidecarKernel,
        process: &mut ActiveProcess,
        operation: IdentityOperation,
    ) -> Result<HostCallReply, HostServiceError> {
        let pid = process.kernel_pid;
        let value = match operation {
            IdentityOperation::GetId { kind } => match kind {
                IdentityIdKind::RealUser => json!(kernel
                    .getuid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?),
                IdentityIdKind::EffectiveUser => json!(kernel
                    .geteuid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?),
                IdentityIdKind::SavedUser => {
                    let (_, _, saved) = kernel
                        .getresuid(EXECUTION_DRIVER_NAME, pid)
                        .map_err(kernel_host_error)?;
                    json!(saved)
                }
                IdentityIdKind::RealGroup => json!(kernel
                    .getgid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?),
                IdentityIdKind::EffectiveGroup => json!(kernel
                    .getegid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?),
                IdentityIdKind::SavedGroup => {
                    let (_, _, saved) = kernel
                        .getresgid(EXECUTION_DRIVER_NAME, pid)
                        .map_err(kernel_host_error)?;
                    json!(saved)
                }
            },
            IdentityOperation::GetUserIds => {
                let (real, effective, saved) = kernel
                    .getresuid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                json!([real, effective, saved])
            }
            IdentityOperation::GetGroupIds => {
                let (real, effective, saved) = kernel
                    .getresgid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                json!([real, effective, saved])
            }
            IdentityOperation::GetSupplementaryGroups => json!(kernel
                .getgroups(EXECUTION_DRIVER_NAME, pid)
                .map_err(kernel_host_error)?),
            IdentityOperation::SetId { kind, value } => {
                let value = value
                    .ok_or_else(|| HostServiceError::new("EINVAL", "identity value is required"))?;
                match kind {
                    IdentityIdKind::RealUser => kernel.setuid(EXECUTION_DRIVER_NAME, pid, value),
                    IdentityIdKind::EffectiveUser => {
                        kernel.seteuid(EXECUTION_DRIVER_NAME, pid, value)
                    }
                    IdentityIdKind::RealGroup => kernel.setgid(EXECUTION_DRIVER_NAME, pid, value),
                    IdentityIdKind::EffectiveGroup => {
                        kernel.setegid(EXECUTION_DRIVER_NAME, pid, value)
                    }
                    IdentityIdKind::SavedUser | IdentityIdKind::SavedGroup => {
                        return Err(HostServiceError::new(
                            "EINVAL",
                            "saved IDs require a setres operation",
                        ));
                    }
                }
                .map_err(kernel_host_error)?;
                Value::Null
            }
            IdentityOperation::SetRealEffectiveUserIds { real, effective } => {
                kernel
                    .setreuid(EXECUTION_DRIVER_NAME, pid, real, effective)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            IdentityOperation::SetUserIds {
                real,
                effective,
                saved,
            } => {
                kernel
                    .setresuid(EXECUTION_DRIVER_NAME, pid, real, effective, saved)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            IdentityOperation::SetRealEffectiveGroupIds { real, effective } => {
                kernel
                    .setregid(EXECUTION_DRIVER_NAME, pid, real, effective)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            IdentityOperation::SetGroupIds {
                real,
                effective,
                saved,
            } => {
                kernel
                    .setresgid(EXECUTION_DRIVER_NAME, pid, real, effective, saved)
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            IdentityOperation::SetSupplementaryGroups { groups } => {
                kernel
                    .setgroups(EXECUTION_DRIVER_NAME, pid, groups.into_vec())
                    .map_err(kernel_host_error)?;
                Value::Null
            }
            IdentityOperation::PasswdById {
                uid,
                max_record_bytes,
            } => account_record(
                kernel
                    .getpwuid_for_process(EXECUTION_DRIVER_NAME, pid, uid)
                    .map_err(kernel_host_error)?,
                max_record_bytes,
            )?,
            IdentityOperation::PasswdByName {
                name,
                max_record_bytes,
            } => account_record(
                kernel
                    .getpwnam_for_process(EXECUTION_DRIVER_NAME, pid, name.as_str())
                    .map_err(kernel_host_error)?,
                max_record_bytes,
            )?,
            IdentityOperation::NextPasswd {
                index,
                max_record_bytes,
            } => account_record(
                kernel
                    .getpwent_for_process(EXECUTION_DRIVER_NAME, pid, index)
                    .map_err(kernel_host_error)?,
                max_record_bytes,
            )?,
            IdentityOperation::GroupById {
                gid,
                max_record_bytes,
            } => account_record(
                kernel
                    .getgrgid_for_process(EXECUTION_DRIVER_NAME, pid, gid)
                    .map_err(kernel_host_error)?,
                max_record_bytes,
            )?,
            IdentityOperation::GroupByName {
                name,
                max_record_bytes,
            } => account_record(
                kernel
                    .getgrnam_for_process(EXECUTION_DRIVER_NAME, pid, name.as_str())
                    .map_err(kernel_host_error)?,
                max_record_bytes,
            )?,
            IdentityOperation::NextGroup {
                index,
                max_record_bytes,
            } => account_record(
                kernel
                    .getgrent_for_process(EXECUTION_DRIVER_NAME, pid, index)
                    .map_err(kernel_host_error)?,
                max_record_bytes,
            )?,
            IdentityOperation::Get => {
                let (uid, euid, suid) = kernel
                    .getresuid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                let (gid, egid, sgid) = kernel
                    .getresgid(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                let groups = kernel
                    .getgroups(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_host_error)?;
                json!({
                    "uid": uid,
                    "euid": euid,
                    "suid": suid,
                    "gid": gid,
                    "egid": egid,
                    "sgid": sgid,
                    "groups": groups,
                })
            }
            other => return Err(unsupported("identity", other)),
        };
        Ok(HostCallReply::Json(value))
    }
}

fn account_record(record: String, maximum: BoundedUsize) -> Result<Value, HostServiceError> {
    if record.len() > maximum.get() {
        return Err(HostServiceError::new(
            "E2BIG",
            format!(
                "account record is {} bytes, exceeding maxAccountRecordBytes ({})",
                record.len(),
                maximum.get()
            ),
        )
        .with_details(json!({
            "limitName": "maxAccountRecordBytes",
            "limit": maximum.get(),
            "requested": record.len(),
        })));
    }
    Ok(Value::String(record))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_record_accepts_exact_limit_and_rejects_limit_plus_one() {
        let payload_limit = PayloadLimit::new("maxAccountRecordBytes", 4_096).unwrap();
        let maximum = BoundedUsize::try_new(4_096, &payload_limit).unwrap();
        assert_eq!(
            account_record("x".repeat(4_096), maximum).unwrap(),
            Value::String("x".repeat(4_096))
        );

        let error = account_record("x".repeat(4_097), maximum).unwrap_err();
        assert_eq!(error.code, "E2BIG");
        let details = error.details.expect("typed limit details");
        assert_eq!(details["limitName"], "maxAccountRecordBytes");
        assert_eq!(details["limit"], 4_096);
        assert_eq!(details["requested"], 4_097);
    }
}
