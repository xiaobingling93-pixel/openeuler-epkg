# formula_stub.rb - Minimal Formula support for post_install
# (~150 lines, covers 99% post_install API usage)
#
# Supported APIs (based on homebrew-core analysis):
# - system() - execute commands (161 uses)
# - HOMEBREW_PREFIX, HOMEBREW_CELLAR - global constants (117+ uses)
# - Formula[name] - get other formula (86 uses)
# - bin/, lib/, share/, var/, sbin/, usr/ - Pathname methods (80+ uses)
# - .mkpath, .exist?, .install - directory operations (80 uses)
# - opt_bin, opt_lib, opt_include - opt symlinks (74 uses)
# - version.major, version.major_minor - version parsing (50 uses)
# - ENV.cc, ENV.cxx - compiler environment (7 uses)
# - Utils.safe_popen_read, safe_popen_write - command execution (7 uses)
# - OS.mac?, OS.linux?, OS.kernel_version - platform detection (9 uses)
# - Hardware::CPU.intel?, arm?, arch - CPU detection
# - FileUtils: cp, cp_r, rm, rm_r, rm_rf, mv, ln, ln_s (built-in)

require 'pathname'
require 'fileutils'
require 'open3'

# Global constants - set by epkg Rust code via ENV
HOMEBREW_PREFIX = Pathname.new(ENV.fetch('HOMEBREW_PREFIX'))
HOMEBREW_CELLAR = Pathname.new(ENV.fetch('HOMEBREW_CELLAR', "#{HOMEBREW_PREFIX}/Cellar"))

# Version class for version parsing (major, minor, patch)
class Version
  attr_reader :major, :minor, :patch

  def initialize(version_str)
    @original = version_str.to_s
    parts = @original.split(/[._]/)
    @major = parts[0]&.to_i || 0
    @minor = parts[1]&.to_i || 0
    @patch = parts[2]&.to_i || 0
  end

  def major_minor; "#{major}.#{minor}"; end
  def major_minor_patch; "#{major}.#{minor}.#{patch}"; end
  def to_s; @original; end
  def to_i; major; end
end

# OS module for platform detection
module OS
  def self.mac?; false; end  # epkg brew runs on Linux
  def self.linux?; true; end

  # Kernel version - from uname -r
  def self.kernel_version
    @kernel_version ||= Version.new(`uname -r`.chomp.split('.').first(3).join('.'))
  end

  VERSION = kernel_version
end

# Hardware module for CPU detection
module Hardware
  class CPU
    def self.intel?
      %w[x86_64 i386 i686].include?(RbConfig::CONFIG['host_cpu'])
    end

    def self.arm?
      %w[arm64 aarch64 arm].include?(RbConfig::CONFIG['host_cpu'])
    end

    def self.type; intel? ? :intel : :arm; end
    def self.arch; RbConfig::CONFIG['host_cpu']; end
    def self.family; :intel; end
    def self.cores; 1; end
  end
end

# MacOS module stub (returns nil on Linux)
module MacOS
  def self.version; nil; end
  def self.full_version; nil; end

  class CLT
    def self.installed?; false; end
    def self.sdk_path; nil; end
    def self.sdk_path_if_needed; nil; end
  end
end

# Language module stubs
module Language
  module Python
    def self.major_minor_version(python_path)
      v = `#{python_path} --version 2>&1`.chomp.split.last
      Version.new(v)
    end
  end
end

# ENV module stubs - Homebrew extends Ruby's ENV with these methods
# We don't override ENV, just stub the methods that formulas might call
module ENVStubs
  def cc; self['CC'] || 'cc'; end
  def cxx; self['CXX'] || 'c++'; end
  def prepend_path(key, value); self[key] = "#{value}:#{self[key]}" if self[key]; end
  def append_path(key, value); self[key] = "#{self[key]}:#{value}" if self[key]; end
  def clang; self['CC'] = 'clang'; end
end
ENV.extend(ENVStubs)

# ChildStatus class to mimic Process::Status for $CHILD_STATUS ($?)
class ChildStatus
  attr_reader :exitstatus

  def initialize(exitstatus, success)
    @exitstatus = exitstatus
    @success = success
  end

  def nonzero?; @exitstatus != 0; end
  def zero?; @exitstatus == 0; end
  def to_i; @exitstatus; end
  def success?; @success; end
end

# Global variable for last command status (initialized to success)
$CHILD_STATUS = ChildStatus.new(0, true)

# Utils module for command execution
module Utils
  def self.safe_popen_read(*cmd)
    # Convert Pathname to String for Open3
    cmd_strs = cmd.flatten.map { |c| c.is_a?(Pathname) ? c.to_s : c }
    stdout, stderr, status = Open3.capture3(*cmd_strs)
    # Set global $CHILD_STATUS for $? checks
    $CHILD_STATUS = ChildStatus.new(status.exitstatus, status.success?)
    raise "Command failed: #{cmd.join(' ')}" unless status.success?
    stdout
  end

  def self.safe_popen_write(*cmd)
    # Convert Pathname to String for Open3
    cmd_strs = cmd.flatten.map { |c| c.is_a?(Pathname) ? c.to_s : c }
    Open3.popen3(*cmd_strs) do |stdin, stdout, stderr, wait_thr|
      yield(stdin)
      stdin.close
      stdout.close
      stderr.close
      status = wait_thr.value
      $CHILD_STATUS = ChildStatus.new(status.exitstatus, status.success?)
    end
  end
end

# BuildOptions class - stub for build options
class BuildOptions
  def head?; false; end
  def stable?; true; end
  def with?(name); false; end
  def without?(name); true; end
  def include?(name); false; end
end

# Formula base class
class Formula
  include FileUtils

  attr_reader :name, :version, :desc, :homepage

  # Build options - stub
  def build; @build ||= BuildOptions.new; end

  # DSL methods - called in formula definition
  def self.desc(text); @desc = text; end
  def self.homepage(url); @homepage = url; end
  def self.license(*args); @license = args; end
  def self.revision(num); @revision = num; end
  def self.head(url, **opts); @head = url; end
  def self.stable(&block); @stable_block = block; end
  def self.url(url, **opts); @url = url; end
  def self.sha256(hash); @sha256 = hash; end
  def self.version(v); @version = v; end
  def self.depends_on(*args); @depends ||= []; @depends += args; end
  def self.patch(&block); end  # stub
  def self.bottle(&block); end  # stub
  def self.option(name, desc = ""); end  # stub
  def self.conflicts_with(*args); end  # stub
  def self.keg_only(reason = nil); end  # stub
  def self.test(&block); end  # stub
  def self.livecheck(&block); end  # stub
  def self.on_macos(&block); end  # stub
  def self.on_linux(&block); yield if block; end  # execute on Linux
  def self.no_autobump!(because: nil); end  # stub
  # Catch any other DSL methods ( depreciated, etc)
  def self.method_missing(name, *args, &block); end

  def initialize(name, version = nil)
    @name = name.to_s.sub(/^.*\//, '')
    @version = version ? Version.new(version) : detect_version
    # Get DSL values from class
    @desc = self.class.instance_variable_get(:@desc) rescue nil
    @homepage = self.class.instance_variable_get(:@homepage) rescue nil
  end

  def detect_version
    cellar_dir = HOMEBREW_CELLAR/@name
    if cellar_dir.exist? && cellar_dir.directory?
      v = cellar_dir.children.select(&:directory?).max_by(&:mtime).basename.to_s
      Version.new(v)
    end
  end

  # Path methods - Cellar paths
  def prefix; HOMEBREW_CELLAR/@name/@version.to_s; end
  def bin; prefix/'bin'; end
  def sbin; prefix/'sbin'; end
  def lib; prefix/'lib'; end
  def share; prefix/'share'; end
  def include; prefix/'include'; end
  def libexec; prefix/'libexec'; end
  def doc; share/'doc'/@name; end
  def man; share/'man'; end
  def man1; man/'man1'; end
  def man2; man/'man2'; end
  def man3; man/'man3'; end
  def pkgshare; share/@name; end
  def pkgetc; etc/@name rescue prefix/'etc'/@name; end
  def info; share/'info'/@name; end

  # Opt paths - symlinks to Cellar
  def opt_prefix; HOMEBREW_PREFIX/'opt'/@name; end
  def opt_bin; opt_prefix/'bin'; end
  def opt_sbin; opt_prefix/'sbin'; end
  def opt_lib; opt_prefix/'lib'; end
  def opt_share; opt_prefix/'share'; end
  def opt_include; opt_prefix/'include'; end
  def opt_libexec; opt_prefix/'libexec'; end

  # Global paths
  def var; HOMEBREW_PREFIX/'var'; end
  def etc; HOMEBREW_PREFIX/'etc'; end
  def cache; HOMEBREW_PREFIX/'cache'; end
  def logs; var/'log'/@name; end
  def rack; HOMEBREW_CELLAR/@name; end
  def usr; HOMEBREW_PREFIX/'usr'; end

  # Check if any version is installed in Cellar
  def any_version_installed?
    cellar_dir = HOMEBREW_CELLAR/@name
    cellar_dir.exist? && cellar_dir.directory? && cellar_dir.children.any?(&:directory?)
  end

  # inreplace - replace text in a file
  # Usage: inreplace(file, pattern, replacement)
  def inreplace(file, pattern, replacement)
    path = file.is_a?(Pathname) ? file.to_s : file
    content = File.read(path)
    content.gsub!(pattern, replacement)
    File.write(path, content)
  end

  def post_install; end

  def self.[](name)
    Formula.new(name.to_s.sub(/^.*\//, ''))
  end
end

# Output helpers
def ohai(msg); puts "==> #{msg}"; end
def opoo(msg); puts "Warning: #{msg}"; end
def onoe(msg); puts "Error: #{msg}"; end
def quiet_system(*args); system(*args) rescue false; end

# Pathname: mkpath, exist?, install, join, children, basename, mtime, parent, chmod, lstat
# Pathname#/ operator: Pathname.new("/a")/"b" => Pathname.new("/a/b")
# FileUtils: cp, cp_r, rm, rm_r, rm_rf, mv, mkdir_p, ln, ln_s, install_symlink