extern crate cpuprofiler;

use super::*;
use self::cpuprofiler::PROFILER;

#[test]
fn profile() {
    // let user = users::get_user_by_uid(users::get_effective_uid()).unwrap();
    let app = App::from(args().get_matches_from(vec![
        "userscan",
        "-vc",
        "/tmp/userscan.profile.cache",
        "../..",
    ]));

    PROFILER
        .lock()
        .unwrap()
        .start("/tmp/userscan.profile")
        .expect("Couldn't start profiler");
    app.run().expect("app run failed");
    PROFILER.lock().unwrap().stop().expect(
        "Couldn't stop profiler",
    );
}
