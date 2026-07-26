#![allow(dead_code)]

mod bootstrap {
    include!("../docs/templates/bootstrap.rs");
}

mod application {
    include!("../docs/templates/application.rs");
}

mod local_database_test {
    include!("../docs/templates/local-database-test.rs");
}

mod toasty_model {
    include!("../docs/templates/toasty-model.rs");

    #[test]
    fn registers_complete_model_set() {
        let _ = model_set();
    }
}
