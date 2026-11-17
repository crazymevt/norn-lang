# quire

Makes a web site from a folder of Markdown posts: a page per post, the front
page, a page per tag, an RSS feed and a sitemap.

```text
site/
  site.edn        {:title "…" :author "…" :url "https://…" :description "…"}
  posts/*.md      Markdown, with YAML front matter (title, date, tags, draft)
  static/         copied as it is (images get their width and height on <img>)
```

```sh
norn run --dir site new "A Post"       # start one (a draft)
norn run --dir site build --zip        # into site/_site, with site.zip of it all
norn run --dir site serve -p 8000      # build, serve, rebuild on save, browsers reload
norn test                              # its tests (tests/site is a sample site)
norn package                           # a command-line archive in dist/
```

A post that can't be read (front matter that isn't YAML, no title, a date
that isn't one) becomes a page saying so, or is left out, or (`--strict`)
stops the build. Only files that changed are written, and files the site no
longer makes are removed. Pages are made with
[norn-html](https://github.com/crazymevt/norn-html), a Git dependency pinned in `norn.lock`.
