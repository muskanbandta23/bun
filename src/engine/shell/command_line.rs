use std::ffi::OsString;

use bun_engine_vm::Options;

const USAGE: &str = "\
Usage: engine-shell [options] [files] [-- arguments]
  -d                          Dumps bytecode and disassembly
  -s                          Compiles on the main thread
  -e <code>                   Evaluates the argument as a script
  -f <file>                   Runs the file
  -h, --help                  Prints this message
  -i                          Reads lines from the standard input (the default without files)
  -m                          Runs everything as a module
  -x                          Prints the exit code
  --test262-async             Expects `print('Test262:AsyncTestComplete')`
  --strict-file=<file>        Runs the file as strict code
  --module-file=<file>        Runs the file as a module, as for any file that ends in .mjs
  --exception=<name>          Expects the last script to throw an instance of that class
  --dumpException             Prints that exception
  --ignoreUncaughtExceptions  Succeeds whatever is thrown
  --watchdog=<milliseconds>   Terminates execution after that long
  --watchdog-exception-ok     Succeeds if execution is terminated
  --can-block-is-false        Makes `Atomics.wait` throw on the main thread
  --destroy-vm                Frees the heap before exiting
  --footprint                 Prints the memory footprint before exiting
  --useDollarVM               Defines `$vm`
  --useControlFlowProfiler    Counts the executions of blocks
  --<option>=<value>          Sets an option of the engine. One that is not known is ignored
";

#[derive(Clone)]
pub(crate) enum Script {
    File {
        path: String,
        is_strict: bool,
        is_module: bool,
    },
    Text(String),
}

#[derive(Clone, Default)]
pub(crate) struct CommandLine {
    pub(crate) scripts: Vec<Script>,
    pub(crate) arguments: Vec<String>,
    pub(crate) expected_exception: Option<String>,
    pub(crate) expected_async_passes: u32,
    pub(crate) watchdog_milliseconds: Option<u64>,
    pub(crate) is_worker: bool,
    pub(crate) is_interactive: bool,
    pub(crate) runs_modules: bool,
    pub(crate) prints_exit_code: bool,
    pub(crate) prints_footprint: bool,
    pub(crate) dumps_expected_exception: bool,
    pub(crate) ignores_uncaught_exceptions: bool,
    pub(crate) accepts_termination: bool,
    pub(crate) cannot_block: bool,
    pub(crate) destroys_vm: bool,
    pub(crate) collects_at_end: bool,
    pub(crate) exposes_vm: bool,
    pub(crate) collects_coverage: bool,
}

fn exit_with_usage(code: i32) -> ! {
    eprint!("{USAGE}");
    std::process::exit(code)
}

impl CommandLine {
    pub(crate) fn parse(arguments: impl Iterator<Item = OsString>) -> (CommandLine, Options) {
        let (mut line, mut options) = (CommandLine::default(), Options::default());
        let mut arguments = arguments.map(|it| it.to_string_lossy().into_owned());
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--" => break,
                "-e" => match arguments.next() {
                    Some(text) => line.scripts.push(Script::Text(text)),
                    None => exit_with_usage(1),
                },
                "-f" => match arguments.next() {
                    Some(path) => line.add_file(&path, false, false),
                    None => exit_with_usage(1),
                },
                "-h" | "--help" => exit_with_usage(0),
                "-i" => line.is_interactive = true,
                "-d" => (options.dump_bytecode, options.dump_disassembly) = (true, true),
                "-s" => options.use_concurrent_jit = false,
                "-m" => line.runs_modules = true,
                "-x" => line.prints_exit_code = true,
                "--test262-async" => line.expected_async_passes += 1,
                "--dumpException" => line.dumps_expected_exception = true,
                "--ignoreUncaughtExceptions" => line.ignores_uncaught_exceptions = true,
                "--watchdog-exception-ok" => line.accepts_termination = true,
                "--can-block-is-false" => line.cannot_block = true,
                "--destroy-vm" => line.destroys_vm = true,
                "--footprint" => line.prints_footprint = true,
                _ => match argument.strip_prefix("--") {
                    Some(option) => line.set_option(option, &mut options),
                    None => line.add_file(&argument, false, argument.ends_with(".mjs")),
                },
            }
        }
        line.is_interactive |= line.scripts.is_empty();
        line.arguments = arguments.collect();
        (line, options)
    }

    pub(crate) fn for_worker(&self) -> CommandLine {
        CommandLine {
            is_worker: true,
            destroys_vm: true,
            accepts_termination: self.accepts_termination,
            ignores_uncaught_exceptions: self.ignores_uncaught_exceptions,
            watchdog_milliseconds: self.watchdog_milliseconds,
            exposes_vm: self.exposes_vm,
            ..CommandLine::default()
        }
    }

    fn add_file(&mut self, path: &str, is_strict: bool, is_module: bool) {
        self.scripts.push(Script::File {
            path: path.to_owned(),
            is_strict,
            is_module,
        });
    }

    fn set_option(&mut self, option: &str, options: &mut Options) {
        let (name, value) = option.split_once('=').unwrap_or((option, "true"));
        let is_set = matches!(value, "true" | "1" | "yes");
        match name {
            "strict-file" => self.add_file(value, true, false),
            "module-file" => self.add_file(value, false, true),
            "exception" => self.expected_exception = Some(value.to_owned()),
            "watchdog" => self.watchdog_milliseconds = value.parse().ok().filter(|&it| it != 0),
            "gcAtEnd" => self.collects_at_end = is_set,
            "useDollarVM" => self.exposes_vm = is_set,
            "useControlFlowProfiler" => self.collects_coverage = is_set,
            _ => {
                options.set(name.as_bytes(), value.as_bytes());
            }
        }
    }
}
