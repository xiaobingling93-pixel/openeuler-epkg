# epkg_formula_stub.rb - Minimal Formula support for post_install
# (~120 lines, covers 95%+ post_install API usage)
#
# This file provides essential Formula class and helper methods
# without requiring the full Homebrew Library (~5000+ lines).
#
# Supported APIs (based on homebrew-core analysis):
# - system() - execute commands (161 uses)
# - HOMEBREW_PREFIX, HOMEBREW_CELLAR - global constants (117+ uses)
# - Formula[name] - get other formula (86 uses)
# - bin/, lib/, share/, var/, sbin/ - Pathname methods (80+ uses)
# - .mkpath, .exist?, .install - directory operations (80 uses)
# - opt_bin, opt_lib, opt_include - opt symlinks (74 uses)
# - pkgshare, pkgetc, libexec - special paths (13-17 uses)
# - ENV.cc, ENV.cxx - compiler environment (7 uses)
# - Utils.safe_popen_read, safe_popen_write - command execution (7 uses)
# - OS.mac?, OS.linux? - platform detection (9 uses)
# - Hardware::CPU.intel?, arm? - CPU detection (a few uses)

require 'pathname'
require 'fileutils'
require 'open3'

# Global constants - set by epkg Rust code via ENV
HOMEBREW_PREFIX = Pathname.new(ENV.fetch('HOMEBREW_PREFIX'))
HOMEBREW_CELLAR = Pathname.new(ENV.fetch('HOMEBREW_CELLAR', "#{HOMEBREW_PREFIX}/Cellar"))

# OS module for platform detection
module OS
  def self.mac?; false; end  # epkg brew runs on Linux
  def self.linux?; true; end
end

# Hardware module for CPU detection
module Hardware
  class CPU
    def self.intel?; true; end   # Assume Intel for x86_64
    def self.arm?; false; end
    def self.type; :intel; end
  end
end

# ENV module for compiler environment variables
# Used in formulas like: system ENV.cc, "test.c", "-o", "test"
module ENV
  class << self
    def cc; ENV['CC'] || 'cc'; end
    def cxx; ENV['CXX'] || 'c++'; end
    def prepend_path(key, value); ENV[key] = "#{value}:#{ENV[key]}" if ENV[key]; end
    def delete(key); ENV.delete(key); end
    def exclude?(key); ENV[key].nil? || ENV[key].empty?; end
    def clang; ENV['CC'] = 'clang'; end
    def filter_map; ENV.map { |k, v| yield(k, v) }.compact; end
  end
end

# Utils module for command execution helpers
module Utils
  def self.safe_popen_read(*cmd)
    stdout, stderr, status = Open3.capture3(*cmd.flatten)
    raise "Command failed: #{cmd.join(' ')}" unless status.success?
    stdout
  end

  def self.safe_popen_write(*cmd)
    stdin, stdout, stderr, wait_thr = Open3.popen3(*cmd.flatten)
    yield(stdin)
    stdin.close
    stdout_str = stdout.read
    stderr_str = stderr.read
    stdout.close
    stderr.close
    wait_thr.value
  end
end

# Formula base class
class Formula
  include FileUtils  # provides cp, cp_r, rm, rm_rf, mv, mkdir_p, etc.

  attr_reader :name, :version

  def initialize(name, version = nil)
    @name = name.to_s.sub(/^.*\//, '')  # Strip tap prefix
    @version = version || detect_version
  end

  # Auto-detect version from Cellar directory
  def detect_version
    cellar_dir = HOMEBREW_CELLAR/@name
    if cellar_dir.exist? && cellar_dir.directory?
      versions = cellar_dir.children.select(&:directory?)
      versions.max_by(&:mtime).basename.to_s if versions.any?
    end
  end

  # Path methods - Cellar paths (where actual files are stored)
  def prefix; HOMEBREW_CELLAR/@name/@version; end
  def bin; prefix/'bin'; end
  def sbin; prefix/'sbin'; end
  def lib; prefix/'lib'; end
  def share; prefix/'share'; end
  def include; prefix/'include'; end
  def libexec; prefix/'libexec'; end
  def doc; share/'doc'/@name; end
  def man; share/'man'; end
  def man1; man/'man1'; end
  def pkgshare; share/@name; end
  def pkgetc; etc/@name rescue prefix/'etc'/@name; end
  def info; share/'info'/@name; end

  # Opt paths - symlinks to Cellar (used by Formula["other_pkg"].opt_bin)
  def opt_prefix; HOMEBREW_PREFIX/'opt'/@name; end
  def opt_bin; opt_prefix/'bin'; end
  def opt_sbin; opt_prefix/'sbin'; end
  def opt_lib; opt_prefix/'lib'; end
  def opt_share; opt_prefix/'share'; end
  def opt_include; opt_prefix/'include'; end
  def opt_libexec; opt_prefix/'libexec'; end

  # Global paths (shared across all packages)
  def var; HOMEBREW_PREFIX/'var'; end
  def etc; HOMEBREW_PREFIX/'etc'; end
  def cache; HOMEBREW_PREFIX/'cache'; end
  def logs; var/'log'/@name; end
  def rack; HOMEBREW_CELLAR/@name; end

  # Default empty implementation - overridden by formula
  def post_install; end

  # Class method: get other formula by name
  # Used like: Formula["glib"].opt_bin/"glib-compile-schemas"
  def self.[](name)
    Formula.new(name.to_s.sub(/^.*\//, ''))
  end
end

# Output helper methods (used by some formulas)
def ohai(msg); puts "==> #{msg}"; end
def opoo(msg); puts "Warning: #{msg}"; end
def onoe(msg); puts "Error: #{msg}"; end
def quiet_system(*args); system(*args) rescue false; end

# Pathname extensions
# Pathname#/ operator is built-in: Pathname.new("/a")/"b" => Pathname.new("/a/b")
# Pathname provides: mkpath, exist?, install, join, children, basename, mtime, parent
# FileUtils provides: cp, cp_r, rm, rm_rf, mv, mkdir_p, ln, ln_s