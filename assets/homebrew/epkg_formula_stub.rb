# epkg_formula_stub.rb - Minimal Formula support for post_install
# (~60 lines, covers 90%+ post_install API usage)
#
# This file provides essential Formula class and helper methods
# without requiring the full Homebrew Library (~5000+ lines).
#
# Supported APIs (based on homebrew-core analysis):
# - system() - execute commands (161 uses)
# - HOMEBREW_PREFIX, HOMEBREW_CELLAR - global constants (117+ uses)
# - Formula[name] - get other formula (86 uses)
# - bin/, lib/, share/, var/ - Pathname methods (80+ uses)
# - .mkpath, .exist? - directory operations (80 uses)
# - opt_bin, opt_lib - opt symlinks (74 uses)
# - pkgshare, pkgetc, libexec - special paths (13-17 uses)

require 'pathname'
require 'fileutils'

# Global constants - set by epkg Rust code via ENV
HOMEBREW_PREFIX = Pathname.new(ENV.fetch('HOMEBREW_PREFIX'))
HOMEBREW_CELLAR = Pathname.new(ENV.fetch('HOMEBREW_CELLAR', "#{HOMEBREW_PREFIX}/Cellar"))

# OS module for platform detection
module OS
  def self.mac?; false; end  # epkg runs on Linux
  def self.linux?; true; end
end

# Formula base class
class Formula
  include FileUtils  # provides cp, cp_r, rm, mv, mkdir_p, etc.

  attr_reader :name, :version

  def initialize(name, version = nil)
    @name = name
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
  def lib; prefix/'lib'; end
  def share; prefix/'share'; end
  def include; prefix/'include'; end
  def libexec; prefix/'libexec'; end
  def pkgshare; share/@name; end
  def pkgetc; prefix/'etc'/@name; end

  # Opt paths - symlinks to Cellar (used by Formula["other_pkg"].opt_bin)
  def opt_prefix; HOMEBREW_PREFIX/'opt'/@name; end
  def opt_bin; opt_prefix/'bin'; end
  def opt_lib; opt_prefix/'lib'; end
  def opt_share; opt_prefix/'share'; end
  def opt_include; opt_prefix/'include'; end
  def opt_libexec; opt_prefix/'libexec'; end

  # Global paths (shared across all packages)
  def var; HOMEBREW_PREFIX/'var'; end
  def etc; HOMEBREW_PREFIX/'etc'; end

  # Default empty implementation - overridden by formula
  def post_install; end

  # Class method: get other formula by name
  # Used like: Formula["glib"].opt_bin/"glib-compile-schemas"
  def self.[](name)
    # Strip tap prefix if present (e.g., "homebrew/core/glib" -> "glib")
    Formula.new(name.to_s.sub(/^.*\//, ''))
  end
end

# Output helper methods (used by some formulas)
def ohai(msg); puts "==> #{msg}"; end
def opoo(msg); puts "Warning: #{msg}"; end
def quiet_system(*args); system(*args) rescue false; end

# Pathname already provides: mkpath, exist?, install, join, children, basename, mtime
# Pathname#/ operator (join) is built-in: Pathname.new("/a")/"b" => Pathname.new("/a/b")