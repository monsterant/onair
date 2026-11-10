fn main() {
    // migrations 폴더가 바뀌면 다시 빌드한다(sqlx::migrate!가 빌드할 때 SQL을 실행 파일에 넣기 때문, #24 A14)
    println!("cargo:rerun-if-changed=migrations");
}
