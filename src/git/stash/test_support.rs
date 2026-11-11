use git2::{Index, IndexEntry, IndexTime, Oid, Repository, Signature};

pub(crate) fn tree(repo: &Repository, entries: &[(&str, Oid, i32)]) -> Oid {
    // An in-memory index builds nested trees without reading or writing the repository index.
    let mut index = Index::new().unwrap();
    for (path, oid, mode) in entries {
        index
            .add(&IndexEntry {
                ctime: IndexTime::new(0, 0),
                mtime: IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: *mode as u32,
                uid: 0,
                gid: 0,
                file_size: 0,
                id: *oid,
                flags: 0,
                flags_extended: 0,
                path: path.as_bytes().to_vec(),
            })
            .unwrap();
    }
    index.write_tree_to(repo).unwrap()
}

pub(crate) fn files(repo: &Repository, entries: &[(&str, &[u8])]) -> Oid {
    let entries: Vec<_> = entries
        .iter()
        .map(|(path, bytes)| (*path, repo.blob(bytes).unwrap(), 0o100644))
        .collect();
    tree(repo, &entries)
}

pub(crate) fn commit(repo: &Repository, tree: Oid, parents: &[Oid], message: &str) -> Oid {
    let tree = repo.find_tree(tree).unwrap();
    let parents: Vec<_> = parents
        .iter()
        .map(|oid| repo.find_commit(*oid).unwrap())
        .collect();
    let parents: Vec<_> = parents.iter().collect();
    let signature = Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(None, &signature, &signature, message, &tree, &parents)
        .unwrap()
}

pub(crate) fn stash(
    repo: &Repository,
    base: Oid,
    index_tree: Oid,
    saved_tree: Oid,
    untracked_tree: Option<Oid>,
) -> Oid {
    let index = commit(repo, index_tree, &[base], "index snapshot");
    let mut parents = vec![base, index];
    if let Some(tree) = untracked_tree {
        parents.push(commit(repo, tree, &[], "saved untracked and ignored"));
    }
    commit(repo, saved_tree, &parents, "On main: saved snapshot")
}
