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
    parts = version_str.to_s.split(/[._]/)
    @major = parts[0]&.to_i || 0
    @minor = parts[1]&.to_i || 0
    @patch = parts[2]&.to_i || 0
  end

  def major_minor; "#{major}.#{minor}"; end
  def major_minor_patch; "#{major}.#{minor}.#{patch}"; end
  def to_s; @major.nil? ? "0" : "#{major}.#{minor}.#{patch}"; end
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

# ENV module for compiler environment
module ENV
  class << self
    def cc; ENV['CC'] || 'cc'; end
    def cxx; ENV['CXX'] || 'c++'; end
    def prepend_path(key, value); ENV[key] = "#{value}:#{ENV[key]}" if ENV[key]; end
    def append_path(key, value); ENV[key] = "#{ENV[key]}:#{value}" if ENV[key]; end
    def delete(key); ENV.delete(key); end
    def exclude?(key); ENV[key].nil? || ENV[key].empty?; end
    def clang; ENV['CC'] = 'clang'; end
    def filter_map; ENV.map { |k, v| yield(k, v) }.compact; end
  end
end

# Utils module for command execution
module Utils
  def self.safe_popen_read(*cmd)
    stdout, stderr, status = Open3.capture3(*cmd.flatten)
    raise "Command failed: #{cmd.join(' ')}" unless status.success?
    stdout
  end

  def self.safe_popen_write(*cmd)
    Open3.popen3(*cmd.flatten) do |stdin, stdout, stderr, wait_thr|
      yield(stdin)
      stdin.close
      stdout.close
      stderr.close
      wait_thr.value
    end
  end
end

# Formula base class
class Formula
  include FileUtils

  attr_reader :name, :version

  def initialize(name, version = nil)
    @name = name.to_s.sub(/^.*\//, '')
    @version = version ? Version.new(version) : detect_version
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