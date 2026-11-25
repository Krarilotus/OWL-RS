//! One-shot hooks passed from the preparer's caller to its worker.

use std::cell::RefCell;

pub enum Stage {
    BeforeClaim,
    WaitingForWriter,
    DuringGrounding,
    BeforeInstall,
}

type Hooks = [Option<Box<dyn FnOnce() + Send>>; 4];

thread_local! {
    static HOOKS: RefCell<Hooks> = const { RefCell::new([None, None, None, None]) };
}

pub fn set(stage: Stage, hook: impl FnOnce() + Send + 'static) {
    HOOKS.with_borrow_mut(|hooks| hooks[stage as usize] = Some(Box::new(hook)));
}

pub(super) fn take() -> Hooks {
    HOOKS.with_borrow_mut(std::mem::take)
}

pub(super) fn install(hooks: Hooks) {
    HOOKS.replace(hooks);
}

pub(super) fn run(stage: Stage) {
    if let Some(hook) = HOOKS.with_borrow_mut(|hooks| hooks[stage as usize].take()) {
        hook();
    }
}
