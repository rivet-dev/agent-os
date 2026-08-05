use agentos_vm::{ExecutorRegistry, VmConfig, VmManager};
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    block_on(async {
        let mut manager = VmManager::builder()
            .executors(ExecutorRegistry::empty())
            .build()?;
        let mut vm = manager.create(VmConfig::default().allow_all()).await?;

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
    })
}

fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}
