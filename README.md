# ao3rss but Rust

Heavily based off/inspired by [ao3rss](https://github.com/FalacerSelene/ao3rss).

## Usage

Usage is basically the same as the original ao3rss.

 - `GET /work/<work_id>`: Generates an RSS feed for the given ao3 work, with one item per chapter.
 - (eventually) `GET /series/<series_id>`: Generates an RSS feed for the given ao3 series, with one item per chapter in every work in the series.

## Differences between this and the original ao3rss
 - Feed is generated using a library, instead of templated text.
   - The RSS library in question could probably use some usability improvements, but I still think it's a step up.
 - Scrapes `https://archiveofourown.org/works/<id>?view_full_work=true` instead of `https://archiveofourown.org/works/<id>/navigate`
   - This lets me pull the summaries and content for every chapter, at the cost of having the dates for each individual chapter.
   - In the future, it should be feasible to scrape both and combine the data, but I figured this was enough for the moment.
 - Features a keep-alive interval. Every second that the RSS feed has not been generated, `<!-- keepalive -->` will be sent.
   - This is an ugly hack. There's probably another way around this. (If I knew what it was, I wouldn't be doing this.)
   - This is enabled by the `keepalive` feature, which can be disabled by passing `--no-default-features` to cargo when building.

## TODO
 - Configuration. Everything is kinda hardcoded at the moment.
 - Print a line to stdout per request.
 - Add feeds for series, and possibly other things.
 - Detect one-shots, and remove chapter numbers from titles accordingly.
