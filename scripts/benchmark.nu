#!/usr/bin/env nu

# Benchmark slopcount against tokei, cloc and sloccount.
#
# Clones (or reuses) the Linux kernel and Chromium sources, times all four
# tools over them, records what each one counted, and writes the lot to a
# single JSON file. This is the methodology behind BENCHMARKS.md.
#
#   ./scripts/benchmark.nu --tempdir ~/temp-bench --out results.json
#
# The checkouts are large (Linux ~2 GB, Chromium ~7 GB while cloning) and are
# left in `--tempdir` so repeat runs reuse them. A manifest in each checkout
# records the commit it was taken from, so a reused tree is verified rather
# than assumed.

# Corpora, in the order they are benchmarked.
const CORPORA = [
    {
        name: "linux"
        url: "https://github.com/torvalds/linux.git"
        # Rough size of the working tree once `.git` is dropped, for the
        # free-space check before cloning.
        needs_gb: 6
    }
    {
        name: "chromium"
        url: "https://chromium.googlesource.com/chromium/src.git"
        needs_gb: 20
    }
]

# Written into each checkout so a later run can verify what it is reusing.
const MANIFEST = ".slopcount-bench.json"

def main [
    --tempdir: path                     # where checkouts live; reused across runs
    --out: path = "benchmark-results.json"  # where to write the results
    --corpora: string = "linux,chromium"    # comma-separated subset to run
    --runs-fast: int = 10               # hyperfine runs for slopcount and tokei
    --runs-slow: int = 3                # hyperfine runs for cloc and sloccount
    --skip: string = ""                 # comma-separated tools to leave out
    --slopcount: path                   # slopcount binary (default: ./target/release/slopcount)
    --keep-going                        # carry on when a single tool fails
] {
    if $tempdir == null {
        error make {msg: "--tempdir is required, e.g. --tempdir ~/temp-bench"}
    }

    let skip = ($skip | split row "," | each {|s| $s | str trim} | where {|s| $s != ""})
    let wanted = ($corpora | split row "," | each {|s| $s | str trim} | where {|s| $s != ""})

    # Validate the arguments before probing tools or touching the filesystem.
    let selected = ($CORPORA | where {|c| $c.name in $wanted})
    if ($selected | is-empty) {
        error make {msg: $"no known corpora in '($corpora)'; choose from: ($CORPORA | get name | str join ', ')"}
    }

    let bin = (resolve-slopcount $slopcount)
    let tools = (check-tools $bin $skip)

    mkdir $tempdir
    let root = ($tempdir | path expand)

    print $"slopcount benchmark"
    print $"  workspace: ($root)"
    print $"  tools:     ($tools | get name | str join ', ')"
    print ""

    let results = ($selected | each {|corpus|
        print $"=== ($corpus.name) ==="
        let dir = (prepare-corpus $corpus $root)
        let stats = (tree-stats $dir)
        print $"  ($stats.files) files, ($stats.bytes | into filesize)"

        warm-cache $dir
        let timings = (time-tools $tools $dir $runs_fast $runs_slow $root $keep_going)
        let memory = (measure-memory $tools $dir $root $keep_going)
        let counts = (collect-counts $tools $dir $root $keep_going)

        {
            name: $corpus.name
            url: $corpus.url
            commit: $stats.commit
            files: $stats.files
            bytes: $stats.bytes
            timings: $timings
            memory: $memory
            counts: $counts
        }
    })

    let report = {
        schema: 1
        generated_at: (date now | format date "%+")
        machine: (machine-info)
        tools: $tools
        settings: {
            runs_fast: $runs_fast
            runs_slow: $runs_slow
            skipped: $skip
        }
        corpora: $results
    }

    $report | to json --indent 2 | save --force $out
    print ""
    print $"wrote ($out)"
    summarise $report
}

# --- environment -------------------------------------------------------------

def resolve-slopcount [given: any]: nothing -> path {
    let candidate = if $given != null {
        $given
    } else {
        ([$env.PWD "target" "release" "slopcount"] | path join)
    }
    if not ($candidate | path exists) {
        error make {msg: $"slopcount binary not found at ($candidate). Run `cargo build --release`, or pass --slopcount <path>."}
    }
    $candidate | path expand
}

# Every tool we can find, with its version. Missing ones are reported and
# dropped rather than aborting the run.
def check-tools [bin: path, skip: list<string>]: nothing -> list {
    let all = [
        {name: "slopcount", cmd: $bin,         version_args: ["--version"]}
        {name: "tokei",     cmd: "tokei",      version_args: ["--version"]}
        {name: "cloc",      cmd: "cloc",       version_args: ["--version"]}
        {name: "sloccount", cmd: "sloccount",  version_args: ["--version"]}
    ]

    for required in ["git" "hyperfine"] {
        if (which $required | is-empty) {
            error make {msg: $"($required) is required but not on PATH"}
        }
    }

    let found = ($all | each {|t|
        if $t.name in $skip {
            print $"  skipping ($t.name) \(--skip)"
            null
        } else if ($t.name != "slopcount") and (which $t.cmd | is-empty) {
            print $"  skipping ($t.name) \(not installed)"
            null
        } else {
            {name: $t.name, cmd: $t.cmd, version: (tool-version $t.cmd $t.version_args)}
        }
    } | compact)

    if ($found | is-empty) {
        error make {msg: "no tools available to benchmark"}
    }
    $found
}

def tool-version [cmd: string, args: list<string>]: nothing -> string {
    let res = (run-external $cmd ...$args | complete)
    let text = $"($res.stdout)($res.stderr)"
    let line = ($text | lines | where {|l| $l =~ '[0-9]+\.[0-9]+'} | get 0?)
    if $line == null { "unknown" } else { $line | str trim }
}

def machine-info []: nothing -> record {
    let os = (sys host)
    let cpu = (sys cpu)
    {
        os: $os.name
        os_version: $os.os_version
        arch: (uname | get machine)
        cpu: ($cpu | first | get brand? | default "unknown")
        cores: ($cpu | length)
        memory: (sys mem | get total)
    }
}

# --- corpora -----------------------------------------------------------------

# Clone the corpus if it is absent, verify it if it is already there. Returns
# the checkout directory.
def prepare-corpus [corpus: record, root: path]: nothing -> path {
    let dir = ([$root $corpus.name] | path join)
    let manifest = ([$dir $MANIFEST] | path join)

    if ($manifest | path exists) {
        let recorded = (open $manifest)
        if $recorded.url == $corpus.url {
            print $"  reusing checkout at ($dir) \(commit ($recorded.commit | str substring 0..8))"
            return $dir
        }
        print $"  checkout at ($dir) is for a different URL; re-cloning"
        rm --recursive --force $dir
    } else if ($dir | path exists) {
        print $"  ($dir) exists but has no manifest; re-cloning"
        rm --recursive --force $dir
    }

    require-space $corpus.needs_gb
    print $"  cloning ($corpus.url) ..."
    let res = (^git clone --depth 1 --single-branch $corpus.url $dir | complete)
    if $res.exit_code != 0 {
        error make {msg: $"git clone failed for ($corpus.name): ($res.stderr)"}
    }

    let commit = (^git -C $dir rev-parse HEAD | str trim)

    # Drop `.git` so every tool walks an identical file set and none is charged
    # for VCS metadata. The commit is recorded first so the tree stays
    # identifiable afterwards.
    {url: $corpus.url, commit: $commit, cloned_at: (date now | format date "%+")}
        | to json --indent 2
        | save --force ([$dir $MANIFEST] | path join)
    rm --recursive --force ([$dir ".git"] | path join)

    print $"  cloned at commit ($commit | str substring 0..8)"
    $dir
}

def require-space [gb: int] {
    let free = (^df -g $env.HOME | lines | last | split row -r '\s+' | get 3 | into int)
    if $free < $gb {
        error make {msg: $"need about ($gb) GiB free to clone, but only ($free) GiB available"}
    }
}

def tree-stats [dir: path]: nothing -> record {
    let manifest = ([$dir $MANIFEST] | path join)
    let commit = if ($manifest | path exists) { (open $manifest).commit } else { null }
    # `find` is far quicker than globbing half a million paths through nu.
    # The manifest this script wrote is not part of the corpus.
    let files = (^find $dir -type f -not -name $MANIFEST | lines | length)
    let kb = (^du -sk $dir | split row -r '\s+' | first | into int)
    {commit: $commit, files: $files, bytes: ($kb * 1024)}
}

# Read the tree once so the first timed run is not the one paying for I/O.
def warm-cache [dir: path] {
    print "  warming page cache ..."
    ^tar --create --file /dev/null $dir out+err> /dev/null
}

# --- the command each tool runs ----------------------------------------------

# slopcount's defaults skip files over 2 MB and treat doc strings as comments.
# Both are switched off here so it does the same work, and counts the same
# things, as the tools it is being compared with.
def tool-command [tool: record, dir: path]: nothing -> string {
    match $tool.name {
        "slopcount" => $"($tool.cmd) ($dir) --max-file-size 0 --doc-strings-as-code"
        "tokei" => $"tokei ($dir)"
        "cloc" => $"cloc ($dir) --quiet"
        "sloccount" => $"sloccount ($dir)"
        _ => (error make {msg: $"no command for ($tool.name)"})
    }
}

def runs-for [tool: record, fast: int, slow: int]: nothing -> int {
    if $tool.name in ["slopcount" "tokei"] { $fast } else { $slow }
}

# --- timing ------------------------------------------------------------------

def time-tools [
    tools: list, dir: path, fast: int, slow: int, root: path, keep_going: bool
]: nothing -> list {
    $tools | each {|tool|
        let runs = (runs-for $tool $fast $slow)
        let cmd = (tool-command $tool $dir)
        print $"  timing ($tool.name) \(($runs) runs) ..."

        let json = ([$root $"hyperfine-($tool.name).json"] | path join)
        let warmup = if $runs > 3 { ["--warmup" "2"] } else { ["--warmup" "1"] }
        let args = ([
            ...$warmup
            "--runs" ($runs | into string)
            "--export-json" $json
            "--command-name" $tool.name
            $cmd
        ])
        let res = (run-external "hyperfine" ...$args | complete)

        if $res.exit_code != 0 or not ($json | path exists) {
            let msg = $"  hyperfine failed for ($tool.name): ($res.stderr | str trim)"
            if $keep_going { print $msg; null } else { error make {msg: $msg} }
        } else {
            let r = (open $json | get results | first)
            print $"    ($r.mean | math round --precision 2)s ± ($r.stddev | math round --precision 2)"
            {
                tool: $tool.name
                command: $cmd
                runs: ($r.times | length)
                mean: $r.mean
                stddev: $r.stddev
                median: $r.median
                min: $r.min
                max: $r.max
                user: $r.user
                system: $r.system
                times: $r.times
            }
        }
    } | compact
}

# --- memory ------------------------------------------------------------------

# One extra run per tool under `/usr/bin/time`, for peak resident set size.
def measure-memory [tools: list, dir: path, root: path, keep_going: bool]: nothing -> list {
    let linux_style = ((uname | get kernel-name) != "Darwin")
    $tools | each {|tool|
        print $"  measuring ($tool.name) memory ..."
        let flag = if $linux_style { "-v" } else { "-l" }
        let parts = (tool-command $tool $dir | split row " ")
        let res = (run-external "/usr/bin/time" $flag ...$parts | complete)
        let bytes = (parse-max-rss $res.stderr $linux_style)
        if $bytes == null {
            let msg = $"  could not read peak RSS for ($tool.name)"
            if $keep_going { print $msg; null } else { error make {msg: $msg} }
        } else {
            print $"    ($bytes | into filesize)"
            {tool: $tool.name, peak_rss: $bytes}
        }
    } | compact
}

def parse-max-rss [text: string, linux_style: bool]: nothing -> any {
    if $linux_style {
        # GNU time: "Maximum resident set size (kbytes): 123456"
        let line = ($text | lines | where {|l| $l =~ "Maximum resident set size"} | get 0?)
        if $line == null { return null }
        ($line | split row ":" | last | str trim | into int) * 1024
    } else {
        # BSD time: "     123456  maximum resident set size"
        let line = ($text | lines | where {|l| $l =~ "maximum resident set size"} | get 0?)
        if $line == null { return null }
        ($line | str trim | split row -r '\s+' | first | into int)
    }
}

# --- counts ------------------------------------------------------------------

# What each tool actually counted, so the numbers can be compared and not just
# the clock.
def collect-counts [tools: list, dir: path, root: path, keep_going: bool]: nothing -> record {
    mut out = {}
    for tool in $tools {
        print $"  collecting ($tool.name) counts ..."
        let counts = (try {
            match $tool.name {
                "slopcount" => (counts-slopcount $tool.cmd $dir)
                "tokei" => (counts-tokei $dir)
                "cloc" => (counts-cloc $dir)
                "sloccount" => (counts-sloccount $dir)
                _ => null
            }
        } catch {|e|
            let msg = $"  could not collect ($tool.name) counts: ($e.msg)"
            if $keep_going { print $msg; null } else { error make {msg: $msg} }
        })
        if $counts != null {
            $out = ($out | insert $tool.name $counts)
        }
    }
    $out
}

def counts-slopcount [bin: path, dir: path]: nothing -> record {
    let res = (^$bin $dir --max-file-size 0 --doc-strings-as-code --format json | complete)
    if $res.exit_code != 0 { error make {msg: $res.stderr} }
    let d = ($res.stdout | from json)
    {
        total: {
            code: $d.total.code
            comments: $d.total.comments
            blanks: $d.total.blanks
            files: $d.total.files
        }
        languages: ($d.rows | reduce --fold {} {|r, acc|
            $acc | insert $r.label {code: $r.code, comments: $r.comments, blanks: $r.blanks, files: $r.files}
        })
    }
}

def counts-tokei [dir: path]: nothing -> record {
    let res = (^tokei $dir --output json | complete)
    if $res.exit_code != 0 { error make {msg: $res.stderr} }
    let d = ($res.stdout | from json)

    # tokei's per-language rows exclude lines it moved into an embedded child
    # language, while its Total includes them. Both are kept, separately, so a
    # later comparison is not misled by either.
    let langs = ($d | transpose name v | where name != "Total")
    {
        total: {
            code: $d.Total.code
            comments: $d.Total.comments
            blanks: $d.Total.blanks
            files: ($langs | each {|row| $row.v.reports | length} | math sum)
        }
        languages: ($langs | reduce --fold {} {|row, acc|
            $acc | insert $row.name {
                code: $row.v.code
                comments: $row.v.comments
                blanks: $row.v.blanks
                files: ($row.v.reports | length)
            }
        })
        children: ($langs | each {|row|
            let kids = ($row.v.children? | default {})
            if ($kids | is-empty) { null } else {
                {
                    parent: $row.name
                    moved: ($kids | transpose child reports | each {|c|
                        {
                            child: $c.child
                            code: ($c.reports | each {|r| $r.stats.code} | math sum)
                            files: ($c.reports | length)
                        }
                    })
                }
            }
        } | compact)
    }
}

def counts-cloc [dir: path]: nothing -> record {
    let res = (^cloc $dir --quiet --json | complete)
    if $res.exit_code != 0 { error make {msg: $res.stderr} }
    let d = ($res.stdout | from json)
    let langs = ($d | transpose name v | where name not-in ["SUM" "header"])
    {
        total: {
            code: $d.SUM.code
            comments: $d.SUM.comment
            blanks: $d.SUM.blank
            files: ($d.SUM.nFiles | into int)
        }
        languages: ($langs | reduce --fold {} {|row, acc|
            $acc | insert $row.name {
                code: $row.v.code
                comments: $row.v.comment
                blanks: $row.v.blank
                files: ($row.v.nFiles | into int)
            }
        })
    }
}

# sloccount reports physical SLOC only -- no comment or blank breakdown -- and
# prints per-language totals as `lang: N (pct%)`.
def counts-sloccount [dir: path]: nothing -> record {
    let res = (^sloccount $dir | complete)
    let text = $res.stdout

    let total_line = ($text | lines | where {|l| $l =~ "Total Physical Source Lines"} | get 0?)
    let total = if $total_line == null {
        null
    } else {
        $total_line | split row "=" | last | str trim | str replace --all "," "" | into int
    }

    let langs = ($text | lines
        | each {|l| $l | parse --regex '^(?<name>[a-z0-9+]+):\s+(?<sloc>[0-9]+)\s+\('}
        | flatten
        | reduce --fold {} {|row, acc| $acc | insert $row.name ($row.sloc | into int)})

    {
        total: {sloc: $total}
        languages: $langs
        # sloccount's counters bail on some files; that is part of the result.
        errors: ($res.stderr | lines | where {|l| $l =~ "ERROR"} | length)
    }
}

# --- summary -----------------------------------------------------------------

def summarise [report: record] {
    for corpus in $report.corpora {
        print ""
        print $"($corpus.name) — ($corpus.files) files, ($corpus.bytes | into filesize)"
        $corpus.timings
            | each {|t| {
                tool: $t.tool
                mean: $"($t.mean | math round --precision 2)s"
                stddev: $"($t.stddev | math round --precision 2)"
                runs: $t.runs
            }}
            | print
    }
}
