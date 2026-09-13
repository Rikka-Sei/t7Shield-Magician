//! magi-app 可执行入口：只把控制权交给库（UI 与编排在 `magi-app` 库中，便于无头测试）。

fn main() -> gtk4::glib::ExitCode {
    magi_app::run()
}
