/*
 * gitfeet - atom feed generator for vueBlog compatible repos
 *
 * Copyright (C) 2021 Florian "sp1rit" <sp1ritCS@protonmail.com>
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 * 
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::mem::MaybeUninit;
use std::collections::BTreeMap;
use std::fs::read_dir;
use std::fmt;

use anyhow::Result;
use chrono::{DateTime, TimeZone, FixedOffset, Utc};
use git2::{self as git, Repository, Sort};
use pulldown_cmark::{Parser, Options as MdO, html};
use serde::Serialize;
use tinytemplate::TinyTemplate;

macro_rules! crate_version {
    () => {
        env!("CARGO_PKG_VERSION")
    };
}

#[derive(Clone, Copy)]
struct Time(git::Time);
impl Time {
	fn to_chrono(&self) -> DateTime<FixedOffset> {
		FixedOffset::east(self.0.offset_minutes() * 60).timestamp(self.0.seconds(), 0)
	}
}
impl fmt::Debug for Time {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_fmt(format_args!("Time {}", self.0.seconds()))
	}
}

#[derive(Debug)]
struct BlogPost<'n> {
	path: &'n str,
	initial: Option<Time>,
	latest: MaybeUninit<Time>,
	author: MaybeUninit<(Option<String>, Option<String>)>
}

#[derive(Debug)]
struct BlogPosts<'p> (BTreeMap<&'p str, BlogPost<'p>>);
impl <'n> BlogPosts<'n> {
	fn new() -> Self {
		Self(BTreeMap::new())
	}
	fn insert_uninit(&mut self, path: &'n str) {
		let post = BlogPost {
			path: &path,
			initial: None,
			latest: MaybeUninit::uninit(),
			author: MaybeUninit::uninit()
		};
		self.0.insert(&post.path, post);
	}
	fn get_mut(&mut self, path: &str) -> Option<&mut BlogPost<'n>> {
		self.0.get_mut(path)
	}
	fn get_n_latest(&self, n: usize) -> Vec<&BlogPost> {
		self.0.iter().rev().take(n).map(|(_k, v)| v).collect()
	}
}

#[derive(Debug, Serialize)]
struct AuthorCtx {
	name: String,
	email: String
}

#[derive(Debug, Serialize)]
struct EntryCtx {
	id: String,
	title: String,
	updated: String,
	author: AuthorCtx,
	content: String,
	link: String,
	published: String
}

#[derive(Debug, Serialize)]
struct Context {
	updated: String,
    gfversion: String,
	entries: Vec<EntryCtx>
}

fn main() -> Result<()> {
	let mut posts = BlogPosts::new();
	let owned_paths: Vec<String> = read_dir("content/")?.filter_map(|res| res.map(|entry| entry.path().to_string_lossy().to_string()).ok()).collect();

	owned_paths.iter().for_each(|path| posts.insert_uninit(path));

    // Credits to @Shnatsel on GH; https://github.com/rust-lang/git2-rs/issues/588#issuecomment-856757971
	let repo = Repository::open(".")?;
	let mut revwalk = repo.revwalk()?;
	let mut sort = Sort::TIME;
	sort.insert(Sort::REVERSE);
	revwalk.set_sorting(sort)?;
	revwalk.push_head()?;

	for commit in revwalk.filter_map(|commit| commit.ok()) {
		let commit = repo.find_commit(commit)?;
		if commit.parent_count() == 1 {
			let prev_commit = commit.parent(0)?;
			let tree = commit.tree()?;
			let prev_tree = prev_commit.tree()?;
			let diff = repo.diff_tree_to_tree(Some(&prev_tree), Some(&tree), None)?;
			for delta in diff.deltas() {
				let path = delta.new_file().path().unwrap();
				if let Some(post) = posts.get_mut(&path.to_string_lossy()) {
					let time = Time(commit.time());
					let author = commit.author();
					post.initial.get_or_insert(time);
					unsafe { 
						post.latest.as_mut_ptr().write(time);
						post.author.as_mut_ptr().write((author.name().map(|name| name.to_owned()), author.email().map(|mail| mail.to_owned())));
					}
				}
			}
		}
	}


	let posts = posts.get_n_latest(20);
	
	let current = repo.head()?.peel_to_tree()?;
	
	let mut opts = MdO::empty();
	opts.insert(MdO::ENABLE_TABLES);
	opts.insert(MdO::ENABLE_FOOTNOTES);
	opts.insert(MdO::ENABLE_STRIKETHROUGH);
	opts.insert(MdO::ENABLE_TASKLISTS);
	
	let entries: Vec<EntryCtx> = posts.into_iter().map(|post| {
		let path = std::path::Path::new(post.path);
		let oid = current.get_path(path).unwrap().id();
		let (name, email) = unsafe { &*post.author.as_ptr() };
		
		let file_content = std::fs::read_to_string(path).unwrap();
		let parser = Parser::new_ext(&file_content, opts);
		let mut content = String::new();
		html::push_html(&mut content, parser);
		
		EntryCtx {
			id: format!("https://sp1rit.ml/read/{}", oid),
			title: path.file_stem().unwrap().to_string_lossy().split('.').collect::<Vec<&str>>()[1].to_string(),
			updated: unsafe { &*post.latest.as_ptr() }.to_chrono().to_rfc3339(),
			author: AuthorCtx {
				name: name.as_ref().unwrap().to_string(),
				email: email.as_ref().unwrap().to_string()
			},
			content,
			link: format!("https://sp1rit.ml/read/{}", oid),
			published: post.initial.unwrap().to_chrono().to_rfc3339()
		}
	}).collect();
	
	let template_content = std::fs::read_to_string("feed.xml.in")?;
	let mut tt = TinyTemplate::new();
	tt.add_template("feed", &template_content)?;
	
	let ctx = Context {
		updated: Utc::now().to_rfc3339(),
        gfversion: crate_version!().to_string(),
		entries
	};
	
	let output = tt.render("feed", &ctx)?;
	
	println!("{}", output);
	
	Ok(())
}
