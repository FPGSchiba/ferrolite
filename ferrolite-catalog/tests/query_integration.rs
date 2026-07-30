use ferrolite_catalog::{Catalog, FileTypeChip, LibraryQuery, NewImage, Scope, TagFilter, TagMode};
use ferrolite_image::{Color, FileKind};
use std::collections::BTreeSet;

fn mk_image(cat: &Catalog, folder: i64, name: &str) -> i64 {
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

/// Like `mk_image`, but with `lens`/`aperture`/`focal_length` set so tests can
/// exercise the v7 metadata columns (all `NewImage::failed` rows leave them
/// `None`).
fn mk_image_with_metadata(
    cat: &Catalog,
    folder: i64,
    name: &str,
    lens: Option<&str>,
    aperture: Option<f32>,
    focal_length: Option<f32>,
) -> i64 {
    let mut img = NewImage::failed(folder, name.into(), 1, 1, FileKind::Raw, 0);
    img.lens = lens.map(str::to_string);
    img.aperture = aperture;
    img.focal_length = focal_length;
    cat.upsert_image(&img).unwrap()
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

#[test]
fn lens_filter_matches_only_rows_with_that_lens() {
    let cat = Catalog::open_in_memory().unwrap();
    let f = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    let sigma = mk_image_with_metadata(&cat, f, "a.nef", Some("Sigma 50mm f/1.4"), None, None);
    let _canon = mk_image_with_metadata(&cat, f, "b.nef", Some("Canon 24-70mm f/2.8"), None, None);
    let _no_lens = mk_image_with_metadata(&cat, f, "c.nef", None, None, None);

    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        lens: Some("Sigma 50mm f/1.4".into()),
        ..Default::default()
    };
    let rows = cat.query_images(&q).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![sigma],
        "only the exact-lens row matches; NULL-lens row excluded"
    );
}

#[test]
fn file_type_set_matches_two_kinds_and_excludes_a_third() {
    let cat = Catalog::open_in_memory().unwrap();
    let f = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    let raw = mk_image(&cat, f, "a.nef");
    let jpeg = mk_image(&cat, f, "b.jpg");
    let _png = mk_image(&cat, f, "c.png");

    let mut file_types = BTreeSet::new();
    file_types.insert(FileTypeChip::Raw);
    file_types.insert(FileTypeChip::Jpeg);
    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        file_types,
        ..Default::default()
    };
    let rows = cat.query_images(&q).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 2, "matches raw + jpeg, excludes png");
    assert!(ids.contains(&raw) && ids.contains(&jpeg));
}

#[test]
fn empty_file_type_set_matches_everything() {
    let cat = Catalog::open_in_memory().unwrap();
    let f = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    mk_image(&cat, f, "a.nef");
    mk_image(&cat, f, "b.jpg");
    mk_image(&cat, f, "c.png");

    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        ..Default::default()
    };
    assert_eq!(cat.query_images(&q).unwrap().len(), 3);
}

#[test]
fn aperture_range_includes_boundaries_and_excludes_outside_and_null() {
    let cat = Catalog::open_in_memory().unwrap();
    let f = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    let lo_bound = mk_image_with_metadata(&cat, f, "a.nef", None, Some(2.8), None);
    let hi_bound = mk_image_with_metadata(&cat, f, "b.nef", None, Some(11.0), None);
    let _below = mk_image_with_metadata(&cat, f, "c.nef", None, Some(1.4), None);
    let _above = mk_image_with_metadata(&cat, f, "d.nef", None, Some(22.0), None);
    let _null_aperture = mk_image_with_metadata(&cat, f, "e.nef", None, None, None);

    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        aperture: Some((2.8, 11.0)),
        ..Default::default()
    };
    let rows = cat.query_images(&q).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 2, "only the boundary rows match: {ids:?}");
    assert!(ids.contains(&lo_bound) && ids.contains(&hi_bound));
}

#[test]
fn focal_range_includes_boundaries_and_excludes_outside_and_null() {
    let cat = Catalog::open_in_memory().unwrap();
    let f = cat.upsert_folder(std::path::Path::new("/a"), None).unwrap();
    let lo_bound = mk_image_with_metadata(&cat, f, "a.nef", None, None, Some(24.0));
    let hi_bound = mk_image_with_metadata(&cat, f, "b.nef", None, None, Some(70.0));
    let _below = mk_image_with_metadata(&cat, f, "c.nef", None, None, Some(14.0));
    let _above = mk_image_with_metadata(&cat, f, "d.nef", None, None, Some(200.0));
    let _null_focal = mk_image_with_metadata(&cat, f, "e.nef", None, None, None);

    let q = LibraryQuery {
        scope: Scope::AllPhotographs,
        focal: Some((24.0, 70.0)),
        ..Default::default()
    };
    let rows = cat.query_images(&q).unwrap();
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    assert_eq!(ids.len(), 2, "only the boundary rows match: {ids:?}");
    assert!(ids.contains(&lo_bound) && ids.contains(&hi_bound));
}
