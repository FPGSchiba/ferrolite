use ferrolite_catalog::{Catalog, LibraryQuery, Scope, TagFilter, TagMode};
use ferrolite_image::{Color, FileKind};

fn mk_image(cat: &Catalog, folder: i64, name: &str) -> i64 {
    use ferrolite_catalog::NewImage;
    cat.upsert_image(&NewImage::failed(
        folder,
        name.into(),
        1,
        1,
        FileKind::Raw,
        0,
    ))
    .unwrap()
}

#[test]
fn tag_filter_returns_images_across_folders() {
    let cat = Catalog::open_in_memory().unwrap();
    let f1 = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    let f2 = cat.upsert_folder(std::path::Path::new("/b"), None).unwrap();
    let i1 = mk_image(&cat, f1, "a.nef");
    let i2 = mk_image(&cat, f2, "b.nef");
    let _i3 = mk_image(&cat, f2, "c.nef");
    let tag = cat.create_tag("keeper", Color::default()).unwrap();
    cat.add_tag_to_image(i1, tag).unwrap();
    cat.add_tag_to_image(i2, tag).unwrap();

    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        tags: TagFilter {
            ids: vec![tag],
            mode: TagMode::Any,
        },
        ..Default::default()
    };
    let rows = cat.query_images(&q).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 2, "tag spans two folders");
    assert!(ids.contains(&i1) && ids.contains(&i2));
}
