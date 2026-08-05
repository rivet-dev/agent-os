use agentos_vm_kernel::root_fs::{FilesystemEntry, RootFilesystemSnapshot};

pub const SERVICES_GUEST_PATH: &str = "/etc/services";

/* Baseline IANA names shipped by ordinary minimal Linux images. This is a
 * lowest-precedence root layer: callers may replace it with a distro-specific
 * /etc/services through a lower or bootstrap entry. */
pub const BASELINE_SERVICES: &str = "\
tcpmux 1/tcp\n\
echo 7/tcp\n\
echo 7/udp\n\
discard 9/tcp\n\
discard 9/udp\n\
systat 11/tcp\n\
daytime 13/tcp\n\
daytime 13/udp\n\
netstat 15/tcp\n\
qotd 17/tcp quote\n\
chargen 19/tcp ttytst source\n\
chargen 19/udp ttytst source\n\
ftp-data 20/tcp\n\
ftp 21/tcp\n\
ssh 22/tcp\n\
telnet 23/tcp\n\
smtp 25/tcp mail\n\
time 37/tcp timserver\n\
time 37/udp timserver\n\
domain 53/tcp\n\
domain 53/udp\n\
bootps 67/udp\n\
bootpc 68/udp\n\
tftp 69/udp\n\
http 80/tcp www\n\
kerberos 88/tcp kerberos5 krb5\n\
kerberos 88/udp kerberos5 krb5\n\
pop3 110/tcp pop-3\n\
sunrpc 111/tcp portmapper\n\
sunrpc 111/udp portmapper\n\
auth 113/tcp authentication tap ident\n\
nntp 119/tcp readnews untp\n\
ntp 123/udp\n\
imap 143/tcp imap2\n\
snmp 161/udp\n\
snmptrap 162/udp snmp-trap\n\
bgp 179/tcp\n\
ldap 389/tcp\n\
ldap 389/udp\n\
https 443/tcp\n\
https 443/udp\n\
shell 514/tcp cmd\n\
syslog 514/udp\n\
submission 587/tcp msa\n\
ldaps 636/tcp\n\
rsync 873/tcp\n\
imaps 993/tcp\n\
pop3s 995/tcp\n\
socks 1080/tcp\n\
mysql 3306/tcp\n\
postgresql 5432/tcp postgres\n\
redis 6379/tcp\n\
http-alt 8080/tcp webcache\n";

pub fn default_services_snapshot() -> RootFilesystemSnapshot {
    RootFilesystemSnapshot {
        entries: vec![FilesystemEntry::file(
            SERVICES_GUEST_PATH,
            BASELINE_SERVICES.as_bytes().to_vec(),
        )],
    }
}
