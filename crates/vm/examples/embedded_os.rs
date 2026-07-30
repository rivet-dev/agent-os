use agentos_vm::driver::{DriverConfig, TokioDriver};
use agentos_vm::{ExecutorRegistry, VmConfig, VmManager};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let driver = TokioDriver::process(&DriverConfig::default())?.handle();
    let mut vms = VmManager::builder()
        .driver(driver.clone())
        .executors(ExecutorRegistry::empty())
        .build()?;

    driver.tokio_handle().block_on(async {
        let mut vm = vms.create(VmConfig::default().allow_all()).await?;
        vm.write_file("/workspace/hello.txt", b"hello").await?;
        assert_eq!(
            vm.read_file("/workspace/hello.txt").await?,
            b"hello".to_vec()
        );
        assert!(vm.kernel()?.list_processes().is_empty());
        let snapshot = vm.kernel_mut()?.snapshot_root_filesystem()?;
        assert!(!snapshot.entries.is_empty());
        vm.dispose().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}
