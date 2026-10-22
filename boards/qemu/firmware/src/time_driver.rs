//! SysTick-backed Embassy clock for the QEMU mps2-an385 example.
//!
//! QEMU runs this Cortex-M3 at 25 MHz. A 1 ms periodic interrupt advances
//! the 64-bit clock and services Embassy's timer queue. No other QEMU timer
//! peripheral or application interrupt is needed.

use core::{cell::RefCell, task::Waker};

use cortex_m::peripheral::{SYST, syst::SystClkSource};
use critical_section::Mutex;
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;

const SYSCLK_HZ: u32 = 25_000_000;
const TICK_HZ: u32 = 1_000;

struct State {
    ticks: u64,
    queue: Queue,
}

struct SysTickDriver {
    state: Mutex<RefCell<State>>,
}

impl SysTickDriver {
    const fn new() -> Self {
        Self {
            state: Mutex::new(RefCell::new(State {
                ticks: 0,
                queue: Queue::new(),
            })),
        }
    }

    fn on_tick(&self) {
        critical_section::with(|cs| {
            let mut state = self.state.borrow(cs).borrow_mut();
            state.ticks += 1;
            let now = state.ticks;
            state.queue.next_expiration(now);
        });
    }
}

impl Driver for SysTickDriver {
    fn now(&self) -> u64 {
        critical_section::with(|cs| self.state.borrow(cs).borrow().ticks)
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            let mut state = self.state.borrow(cs).borrow_mut();
            state.queue.schedule_wake(at, waker);
            // Also service deadlines that have passed since the last tick.
            let now = state.ticks;
            state.queue.next_expiration(now);
        });
    }
}

embassy_time_driver::time_driver_impl!(static DRIVER: SysTickDriver = SysTickDriver::new());

pub fn init(mut systick: SYST) {
    systick.set_clock_source(SystClkSource::Core);
    systick.set_reload(SYSCLK_HZ / TICK_HZ - 1);
    systick.clear_current();
    systick.enable_interrupt();
    systick.enable_counter();
}

pub fn on_tick() {
    DRIVER.on_tick();
}
