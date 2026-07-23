fn main() {
    // ui/app.slint 을 컴파일해 `slint::include_modules!()` 로 노출한다.
    slint_build::compile("ui/app.slint").expect("Slint UI 컴파일 실패");

    // Windows: 실행 파일에 아이콘 리소스를 박아 Explorer·작업표시줄·인스톨러에서
    // 아이콘이 보이게 한다. (다른 OS 에선 아무 일도 하지 않는다.)
    // build.rs 는 호스트에서 도므로, Windows 타깃은 windows-latest 러너(호스트=windows)에서
    // 빌드된다 → cfg(windows) 로 분기해도 정상 동작한다.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/assets/ymemo.ico");
        if let Err(e) = res.compile() {
            // 아이콘 리소스가 없어도 빌드 자체는 막지 않는다(경고만).
            println!("cargo:warning=Windows 아이콘 리소스 컴파일 실패: {e}");
        }
    }
}
