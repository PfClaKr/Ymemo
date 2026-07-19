fn main() {
    // ui/app.slint 을 컴파일해 `slint::include_modules!()` 로 노출한다.
    slint_build::compile("ui/app.slint").expect("Slint UI 컴파일 실패");
}
