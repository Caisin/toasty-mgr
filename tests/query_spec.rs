use toasty_mgr::{Deferred, Model, TcQuery};

type CustomerId = i64;
type CustomerMetadata = String;

#[derive(Debug, Model)]
struct SearchCustomer {
    #[key]
    id: i64,
    name: String,
    state: bool,
    created_at: i64,
}

toasty_mgr::tc_query_spec! {
    pub CustomerSearch for SearchCustomer {
        filters {
            id: i64 => id.eq;
            name_prefix: String => name.starts_with;
            state: bool => state.eq;
            created_range: (i64, i64) => created_at.between;
            ids: Vec<i64> => id.in_list;
        }
        sort {
            id;
            name;
            created_at;
        }
        default_order [created_at Desc];
        tie_breaker id Asc;
        page {
            default_size: 20;
            max_size: 100;
        }
    }
}

toasty_mgr::tc_query_spec! {
    CustomerFilter for SearchCustomer {
        filters {
            state: bool => state.eq;
        }
        sort {
            id;
        }
        default_order [id Asc];
        tie_breaker id Asc;
    }
}

toasty_mgr::tc_query_spec! {
    FilterOnly for SearchCustomer {
        filters {
            state: bool => state.eq;
        }
    }
}

toasty_mgr::tc_query_spec! {
    SortOnly for SearchCustomer {
        filters {}
        sort {
            name;
        }
    }
}

toasty_mgr::tc_query_spec! {
    PageOnly for SearchCustomer {
        filters {
            state: bool => state.eq;
        }
        page {
            default_size: 10;
            max_size: 50;
        }
    }
}

#[derive(Debug, Model)]
struct AmbiguousFieldCustomer {
    #[key]
    id: i64,
    r#type: String,
    login_eq_state: bool,
}

toasty_mgr::tc_query_spec! {
    AmbiguousFieldFilter for AmbiguousFieldCustomer {
        filters {
            kind: String => r#type.eq;
            login_state: bool => login_eq_state.eq;
        }
    }
}

#[derive(Debug, Model, TcQuery)]
struct ExtendedCustomer {
    #[key]
    id: i64,
    name: String,
    state: bool,
    created_at: i64,
    nickname: Option<String>,
    #[tc_query(ops(eq, ne), sort)]
    external_id: CustomerId,
    #[tc_query(skip = "JSON filters use Toasty's native expression API")]
    metadata: CustomerMetadata,
}

#[derive(Debug, Model, TcQuery)]
struct Owner {
    #[key]
    id: i64,
}

#[derive(Debug, Model, TcQuery)]
struct OwnedRecord {
    #[key]
    id: i64,
    owner_id: i64,
    #[belongs_to(key = owner_id, references = id)]
    owner: Deferred<Owner>,
}

mod exported_model {
    use toasty_mgr::{Model, TcQuery};

    #[derive(Debug, Model, TcQuery)]
    pub struct ExportedCustomer {
        #[key]
        pub id: i64,
        pub name: String,
    }
}

#[test]
fn query_spec_does_not_require_tc_query_derive() {
    let owned = String::from("Al");
    let request = CustomerSearch::builder()
        .name_prefix(&owned)
        .state(true)
        .asc_name()
        .filter(SearchCustomer::fields().id().gt(0))
        .build();

    let _: toasty_mgr::stmt::Expr<bool> = request.into_expr().unwrap();
    let _: toasty_mgr::schema::QueryMany<SearchCustomer> = CustomerFilter::builder()
        .state(true)
        .build()
        .into_query()
        .unwrap();
    let _ = FilterOnly::builder()
        .state(true)
        .build()
        .into_expr()
        .unwrap();
    let _ = SortOnly::builder().asc_name().build().into_query().unwrap();
    let page_only = PageOnly::default();
    assert_eq!(page_only.page, 1);
    assert_eq!(page_only.size, 10);
}

#[test]
fn query_spec_field_and_operation_are_unambiguous() {
    let _ = AmbiguousFieldFilter::builder()
        .kind("admin")
        .login_state(true)
        .build()
        .into_expr()
        .unwrap();
}

#[test]
fn tc_query_derive_accepts_owned_and_borrowed_strings() {
    let owned = String::from("Alice");
    let _ = ExtendedCustomer::all().name_eq("Alice");
    let _ = ExtendedCustomer::all().name_ne(&owned);
    let _ = ExtendedCustomer::all()
        .name_starts_with(owned)
        .state_eq(true)
        .created_at_ge(1)
        .nickname_is_some()
        .external_id_eq(10)
        .desc_created_at()
        .asc_id();
}

#[test]
fn tc_query_derive_ignores_relationship_fields() {
    let _ = OwnedRecord::all().owner_id_eq(1).asc_owner_id();
}

#[test]
fn tc_query_extension_trait_can_be_imported_across_modules() {
    use exported_model::{ExportedCustomer, ExportedCustomerTcQueryExt};

    let _ = ExportedCustomer::all().name_starts_with("Al").asc_id();
}

#[test]
fn repeated_sort_is_validated_only_at_query_construction() {
    let result = CustomerSearch::builder()
        .asc_name()
        .desc_name()
        .build()
        .into_query();
    let error = match result {
        Ok(_) => panic!("duplicate sort should fail"),
        Err(error) => error,
    };

    assert_eq!(
        error,
        toasty_mgr::TcQueryBuildError::DuplicateSort { field: "name" }
    );
}

#[test]
fn generated_default_uses_declared_page_values() {
    let request = CustomerSearch::default();
    assert_eq!(request.page, 1);
    assert_eq!(request.size, 20);
}

#[cfg(any(feature = "sqlite", feature = "turso"))]
async fn assert_query_spec_backend(code: &str, url: &str) -> anyhow::Result<()> {
    let mut db =
        toasty_mgr::TcMgr::add_by_url_with_models(code, url, toasty_mgr::models!(SearchCustomer))
            .await?;
    db.push_schema().await?;

    for (id, name, state, created_at) in [
        (1, "Alice", true, 30),
        (2, "Alfred", true, 20),
        (3, "Alicia", false, 10),
        (4, "Bob", true, 40),
    ] {
        toasty_mgr::create!(SearchCustomer {
            id,
            name,
            state,
            created_at,
        })
        .exec(&mut db)
        .await?;
    }

    let count = CustomerSearch::builder()
        .name_prefix("Al")
        .page(0)
        .size(0)
        .build()
        .count(&mut db)
        .await?;
    assert_eq!(count, 3);

    let all = CustomerSearch::builder()
        .state(true)
        .page(0)
        .size(0)
        .build()
        .all(&mut db)
        .await?;
    assert_eq!(
        all.iter().map(|customer| customer.id).collect::<Vec<_>>(),
        [4, 1, 2]
    );

    let page = CustomerSearch::builder()
        .state(true)
        .filter(SearchCustomer::fields().id().gt(1))
        .asc_name()
        .page(1)
        .size(1)
        .build()
        .fetch_page(&mut db)
        .await?;
    assert_eq!(page.total, 2);
    assert_eq!(page.total_pages, 2);
    assert_eq!(page.paging.page, 1);
    assert_eq!(page.items[0].name, "Alfred");

    let invalid = CustomerSearch::builder()
        .page(0)
        .build()
        .fetch_page(&mut db)
        .await;
    assert!(matches!(
        invalid,
        Err(toasty_mgr::TcQueryError::Build(
            toasty_mgr::TcQueryBuildError::InvalidPageNumber
        ))
    ));

    toasty_mgr::TcMgr::remove(code);
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn query_spec_executes_on_sqlite() -> anyhow::Result<()> {
    assert_query_spec_backend("query_spec_sqlite", "sqlite::memory:").await
}

#[cfg(feature = "turso")]
#[tokio::test]
async fn query_spec_executes_on_turso() -> anyhow::Result<()> {
    assert_query_spec_backend("query_spec_turso", "turso::memory:").await
}
