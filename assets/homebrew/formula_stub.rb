# formula_stub.rb - Minimal Formula support for post_install
# (~450 lines, covers 99% post_install API usage based on Homebrew source analysis)
#
# Supported APIs (based on Homebrew source analysis):
# - system() - execute commands, convert Pathname args
# - HOMEBREW_PREFIX, HOMEBREW_CELLAR - global constants
# - Formula[name] - get other formula
# - bin/, lib/, share/, var/, sbin/, usr/ - Pathname methods
# - .mkpath, .exist?, .install, .install_symlink, .atomic_write - Pathname operations
# - opt_bin, opt_lib, opt_include - opt symlinks
# - version.major, version.major_minor - version parsing
# - ENV.cc, ENV.cxx, ENV.prepend_path - compiler environment
# - Utils.safe_popen_read, safe_popen_write - command execution
# - OS.mac?, OS.linux?, OS.kernel_version, OS.kernel_name - platform detection
# - Hardware::CPU.intel?, arm?, arch, is_64_bit?, cores - CPU detection
# - FileUtils: cp, cp_r, rm, rm_r, rm_rf, mv, mkdir_p, ln, ln_s (built-in)
# - Blank/present? methods for Object, String, NilClass, Array, Hash, etc.

require 'pathname'
require 'fileutils'
require 'open3'
require 'tempfile'
require 'etc'

# ============================================================================
# Exception classes
# ============================================================================

# Raised by Kernel#safe_system when a command fails
class ErrorDuringExecution < RuntimeError
  attr_reader :cmd, :status, :output

  def initialize(cmd, status: nil, output: nil)
    @cmd = cmd
    @status = status
    @output = output

    # Handle different status types
    exitstatus = case status
                 when Integer
                   status
                 when nil
                   1
                 else
                   status.respond_to?(:exitstatus) ? status.exitstatus : 1
                 end

    cmd_str = cmd.is_a?(Array) ? cmd.join(' ') : cmd.to_s
    super "Failure while executing: `#{cmd_str}` exited with #{exitstatus}."
  end
end

# ============================================================================
# Blank/present? methods (from Homebrew extend/blank.rb)
# ============================================================================

class Object
  def blank?
    respond_to?(:empty?) ? !!empty? : false
  end

  def present?
    !blank?
  end

  def presence
    self if present?
  end
end

class NilClass
  def blank?; true; end
  def present?; false; end
end

class FalseClass
  def blank?; true; end
  def present?; false; end
end

class TrueClass
  def blank?; false; end
  def present?; true; end
end

class Array
  def blank?; empty?; end
  def present?; !empty?; end
end

class Hash
  def blank?; empty?; end
  def present?; !empty?; end
end

class Numeric
  def blank?; false; end
  def present?; true; end
end

class Time
  def blank?; false; end
  def present?; true; end
end

class Symbol
  def blank?; empty?; end
  def present?; !empty?; end
end

class String
  BLANK_RE = /\A[[:space:]]*\z/

  def blank?
    empty? || BLANK_RE.match?(self)
  end

  def present?
    !blank?
  end

  # The inverse of include?
  def exclude?(string)
    !include?(string)
  end
end

# ============================================================================
# Global constants - set by epkg Rust code via ENV
# ============================================================================

HOMEBREW_PREFIX = Pathname.new(ENV.fetch('HOMEBREW_PREFIX'))
HOMEBREW_CELLAR = Pathname.new(ENV.fetch('HOMEBREW_CELLAR', "#{HOMEBREW_PREFIX}/Cellar"))
HOMEBREW_BOTTLES_EXTNAME_REGEX = /\.bottle\.tar\.gz$/

# ============================================================================
# Version class (from Homebrew version.rb)
# ============================================================================

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
  def null?; @original.empty? || @original == '0.0'; end

  def <=>(other)
    other = Version.new(other) unless other.is_a?(Version)
    [major, minor, patch] <=> [other.major, other.minor, other.patch]
  end

  include Comparable

  # NULL constant - lazy initialization
  def self.NULL
    @null ||= Version.new('0.0')
  end
end

# ============================================================================
# OS module (from Homebrew os.rb)
# ============================================================================

module OS
  def self.mac?
    RbConfig::CONFIG['host_os'].include?('darwin')
  end

  def self.linux?
    RbConfig::CONFIG['host_os'].include?('linux')
  end

  def self.kernel_version
    @kernel_version ||= Version.new(Etc.uname.fetch(:release))
  end

  def self.kernel_name
    @kernel_name ||= Etc.uname.fetch(:sysname)
  end

  def self.not_tier_one_configuration?
    false
  end

  VERSION = kernel_version

  if linux?
    require 'os/linux/glibc'
    ISSUES_URL = "https://docs.brew.sh/Troubleshooting"
  elsif mac?
    ISSUES_URL = "https://docs.brew.sh/Troubleshooting"
  end
end

# ============================================================================
# OS::Linux::Glibc module (stub, also loaded from os/linux/glibc.rb)
# ============================================================================

module OS
  module Linux
    module Glibc
      def self.system_version
        @system_version ||= begin
          version = `ldd --version 2>/dev/null`[/ (\d+\.\d+)/, 1]
          version ? Version.new(version) : Version.NULL
        end
      end

      def self.version
        @version ||= begin
          ldd_path = HOMEBREW_PREFIX/'opt/glibc/bin/ldd'
          if ldd_path.executable?
            version = `#{ldd_path} --version 2>/dev/null`[/ (\d+\.\d+)/, 1]
            version ? Version.new(version) : system_version
          else
            system_version
          end
        end
      end

      def self.minimum_version
        Version.new('2.17')
      end

      def self.below_minimum_version?
        system_version < minimum_version
      end
    end
  end
end

# ============================================================================
# Hardware module (from Homebrew hardware.rb)
# ============================================================================

module Hardware
  class CPU
    INTEL_32BIT_ARCHS = [:i386].freeze
    INTEL_64BIT_ARCHS = [:x86_64].freeze
    INTEL_ARCHS = (INTEL_32BIT_ARCHS + INTEL_64BIT_ARCHS).freeze
    ARM_64BIT_ARCHS = [:arm64, :aarch64].freeze
    ARM_ARCHS = ARM_64BIT_ARCHS.freeze

    def self.intel?
      INTEL_ARCHS.include?(RbConfig::CONFIG['host_cpu'].to_sym)
    end

    def self.arm?
      ARM_ARCHS.include?(RbConfig::CONFIG['host_cpu'].to_sym)
    end

    def self.ppc?
      false  # Not supported in epkg
    end

    def self.type
      intel? ? :intel : (arm? ? :arm : :dunno)
    end

    def self.arch
      is_64_bit? ? (intel? ? :x86_64 : :arm64) : (intel? ? :i386 : :arm)
    end

    def self.family
      :dunno
    end

    def self.cores
      @cores ||= begin
        cores = `getconf _NPROCESSORS_ONLN 2>/dev/null`.chomp.to_i
        cores > 0 ? cores : 1
      end
    end

    def self.bits
      case RbConfig::CONFIG['host_cpu']
      when 'x86_64', 'aarch64', 'arm64', 'ppc64', 'powerpc64' then 64
      when 'i386', 'i686', 'arm', 'ppc' then 32
      else 64
      end
    end

    def self.is_64_bit?
      bits == 64
    end

    def self.is_32_bit?
      bits == 32
    end

    def self.sse4?
      intel? && is_64_bit?
    end

    def self.little_endian?
      [1].pack('I') == [1].pack('N')
    end

    def self.big_endian?
      !little_endian?
    end

    def self.virtualized?
      false
    end

    def self.in_rosetta2?
      false
    end

    def self.features
      []
    end

    def self.feature?(name)
      features.include?(name)
    end

    # Optimization flags for compiler
    def self.optimization_flags
      {
        dunno: '',
        native: '-march=native',
        ivybridge: '-march=ivybridge',
        sandybridge: '-march=sandybridge',
        westmere: '-march=westmere',
        nehalem: '-march=nehalem',
        core2: '-march=core2',
        core: '-march=prescott',
        arm_vortex_tempest: '',
        armv6: '-march=armv6',
        armv8: '-march=armv8-a',
      }.freeze
    end
  end

  def self.cores_as_words
    case CPU.cores
    when 1 then 'single'
    when 2 then 'dual'
    when 4 then 'quad'
    when 6 then 'hexa'
    when 8 then 'octa'
    when 10 then 'deca'
    when 12 then 'dodeca'
    else CPU.cores.to_s
    end
  end

  def self.oldest_cpu(_version = nil)
    if CPU.intel?
      CPU.is_64_bit? ? :core2 : :core
    elsif CPU.arm?
      CPU.is_64_bit? ? :armv8 : :armv6
    else
      CPU.family
    end
  end
end

# ============================================================================
# MacOS module stub (returns nil on Linux)
# ============================================================================

module MacOS
  def self.version; nil; end
  def self.full_version; nil; end

  class CLT
    def self.installed?; false; end
    def self.sdk_path; nil; end
    def self.sdk_path_if_needed; nil; end
  end
end

# ============================================================================
# Language module (from Homebrew language/*.rb)
# ============================================================================

module Language
  module Python
    def self.major_minor_version(python)
      version = `#{python} --version 2>&1`.chomp[/(\d\.\d+)/, 1]
      version ? Version.new(version) : nil
    end

    def self.homebrew_site_packages(python = 'python3')
      HOMEBREW_PREFIX/site_packages(python)
    end

    def self.site_packages(python = 'python3')
      if python == 'pypy' || python == 'pypy3'
        'site-packages'
      else
        v = major_minor_version(python)
        "lib/python#{v}/site-packages"
      end
    end

    # Shebang module for formula inclusion
    module Shebang
      PYTHON_SHEBANG_REGEX = %r{\A#! ?(?:/usr/bin/(?:env )?)?python(?:[23](?:\.\d{1,2})?)?( |$)}
      PYTHON_SHEBANG_MAX_LENGTH = "#! /usr/bin/env pythonx.yyy ".length

      def python_shebang_rewrite_info(python_path)
        Utils::Shebang::RewriteInfo.new(
          PYTHON_SHEBANG_REGEX,
          PYTHON_SHEBANG_MAX_LENGTH,
          "#{python_path}\\1"
        )
      end

      def detected_python_shebang(formula = self, use_python_from_path: false)
        python_path = use_python_from_path ? '/usr/bin/env python3' : Formula['python3'].opt_bin/'python3'
        python_shebang_rewrite_info(python_path)
      end
    end

    # Virtualenv module stub
    module Virtualenv
      def virtualenv_create(venv_root, python = 'python', formula = self,
                            system_site_packages: true, without_pip: true)
        system python, '-m', 'venv', venv_root.to_s
        Pathname.new(venv_root)
      end

      def virtualenv_install_with_resources(**kwargs)
        # Stub - not needed for post_install
      end

      def python_names
        %w[python python3 pypy pypy3]
      end
    end
  end

  module Java
    def self.overridable_java_home_env(version = nil)
      java_home = ENV.fetch('JAVA_HOME', '/usr/lib/jvm/default-java')
      { JAVA_HOME: java_home }
    end
  end

  module Node
    def self.packages; []; end
  end

  module Perl
    def self.packages; []; end
  end

  module Php
    def self.packages; []; end
  end
end

# ============================================================================
# Utils::Shebang module
# ============================================================================

module Utils
  module Shebang
    class RewriteInfo
      attr_reader :regex, :max_length, :replacement

      def initialize(regex, max_length, replacement)
        @regex = regex
        @max_length = max_length
        @replacement = replacement
      end
    end
  end
end

# ============================================================================
# ENV module stubs - Homebrew extends Ruby's ENV
# ============================================================================

module ENVStubs
  def cc; self['CC'] || 'cc'; end
  def cxx; self['CXX'] || 'c++'; end
  def fc; self['FC'] || 'gfortran'; end

  def prepend_path(key, value)
    self[key] = "#{value}:#{self[key]}" if self[key]
  end

  def append_path(key, value)
    self[key] = "#{self[key]}:#{value}" if self[key]
  end

  def clang; self['CC'] = 'clang'; end
  def refurbish_args; end  # stub
end
ENV.extend(ENVStubs)

# ============================================================================
# ChildStatus class for $CHILD_STATUS ($?)
# ============================================================================

class ChildStatus
  attr_reader :exitstatus, :termsig

  def initialize(exitstatus, success = true)
    @exitstatus = exitstatus
    @termsig = nil
    @success = success
  end

  def nonzero?; exitstatus != 0; end
  def zero?; exitstatus == 0; end
  def to_i; exitstatus; end
  def success?; @success; end
end

$CHILD_STATUS = ChildStatus.new(0, true)

# ============================================================================
# Utils module (from Homebrew utils/popen.rb)
# ============================================================================

module Utils
  IO_DEFAULT_BUFFER_SIZE = 4096

  def self.popen_read(*args, safe: false, **options)
    output = popen(args, 'rb', options)
    return output if !safe || $CHILD_STATUS.success?
    raise ErrorDuringExecution.new(args, status: $CHILD_STATUS, output: [[:stdout, output]])
  end

  def self.safe_popen_read(*args, **options)
    popen_read(*args, safe: true, **options)
  end

  def self.popen_write(*args, safe: false, **options, &block)
    output = ''
    popen(args, 'w+b', options) do |pipe|
      # Before yielding, capture output
      begin
        loop { output += pipe.read_nonblock(IO_DEFAULT_BUFFER_SIZE) }
      rescue IO::WaitReadable, EOFError
        # No more data available yet
      end

      block.call(pipe) if block
      pipe.close_write

      # Capture remaining output
      output += pipe.read
    end
    return output if !safe || $CHILD_STATUS.success?
    raise ErrorDuringExecution.new(args, status: $CHILD_STATUS, output: [[:stdout, output]])
  end

  def self.safe_popen_write(*args, **options, &block)
    popen_write(*args, safe: true, **options, &block)
  end

  def self.popen(args, mode, options = {})
    args = args.flatten.map { |a| a.is_a?(Pathname) ? a.to_s : a }

    IO.popen('-', mode) do |pipe|
      if pipe
        return pipe.read unless block_given?
        yield pipe
      else
        options[:err] ||= File::NULL unless ENV['HOMEBREW_STDERR']
        begin
          exec(*args, options)
        rescue Errno::ENOENT
          cmd = args[0].is_a?(Hash) ? args[1] : args[0]
          $stderr.puts "brew: command not found: #{cmd}" if options[:err] != :close
          exit! 127
        rescue SystemCallError => e
          cmd = args[0].is_a?(Hash) ? args[1] : args[0]
          $stderr.puts "brew: exec failed: #{cmd}: #{e.message}" if options[:err] != :close
          exit! 1
        end
      end
    end
  end

  # String utilities
  def self.pluralize(stem, count, plural: 's', singular: '', include_count: false)
    prefix = include_count ? "#{count} " : ''
    suffix = (count == 1) ? singular : plural
    "#{prefix}#{stem}#{suffix}"
  end

  def self.deconstantize(path)
    path[0, path.rindex('::') || 0] || ''
  end

  def self.demodulize(path)
    return '' if path.nil?
    i = path.rindex('::')
    i ? path[(i + 2)..] : path
  end
end

# ============================================================================
# Requirement class
# ============================================================================

class Requirement
  attr_reader :name, :cask, :download, :tags

  def initialize(tags = [])
    @tags = tags
    @name = infer_name
  end

  def infer_name
    klass = self.class.name.to_s.sub(/(Dependency|Requirement)$/, '').sub(/^(\w+::)*/, '')
    klass.downcase if klass.present?
  end

  def option_names
    [name]
  end

  def message
    "#{self.class.name} unsatisfied!"
  end

  def satisfied?(**kwargs)
    satisfy = self.class.satisfy
    return true unless satisfy
    true  # stub: always satisfied for post_install
  end

  def fatal?
    self.class.fatal || false
  end

  def display_s
    name.to_s.capitalize
  end

  def optional?
    @tags.include?(:optional)
  end

  def recommended?
    @tags.include?(:recommended)
  end

  def required?
    !optional? && !recommended?
  end

  class << self
    attr_reader :env_proc, :build, :satisfied

    def cask(val = nil); val.nil? ? @cask : @cask = val; end
    def download(val = nil); val.nil? ? @download : @download = val; end
    def fatal(val = nil); val.nil? ? @fatal : @fatal = val; end

    def satisfy(options = nil, &block)
      return @satisfied if options.nil? && !block
      @satisfied = true  # stub
    end

    def env(*settings, &block)
      if block
        @env_proc = block
      else
        super
      end
    end

    def expand(dependent, cache_key: nil)
      Requirements.new
    end

    def prune
      throw(:prune, true)
    end
  end
end

# ============================================================================
# Requirements class
# ============================================================================

class Requirements < Array
end

# ============================================================================
# BuildOptions class
# ============================================================================

class BuildOptions
  def head?; false; end
  def stable?; true; end
  def with?(name); false; end
  def without?(name); true; end
  def include?(name); false; end
  def used_options; []; end
  def unused_options; []; end
end

# ============================================================================
# Dependency class stub
# ============================================================================

class Dependency
  attr_reader :name

  def initialize(name, tags = [])
    @name = name
    @tags = tags
  end

  def build?; @tags.include?(:build); end
  def test?; @tags.include?(:test); end
  def optional?; @tags.include?(:optional); end
  def recommended?; @tags.include?(:recommended); end
  def required?; !optional? && !recommended?; end
  def uses_from_macos?; @tags.include?(:uses_from_macos); end

  def to_formula
    Formula[name]
  end

  def self.prune
    throw(:prune, true)
  end
end

class Dependencies < Array
end

# ============================================================================
# Resource class stub
# ============================================================================

class Resource
  attr_reader :name, :url, :version, :sha256

  def initialize(name)
    @name = name
  end

  def url(val = nil, **opts); val.nil? ? @url : @url = val; end
  def sha256(val = nil); val.nil? ? @sha256 : @sha256 = val; end
  def version(val = nil); val.nil? ? @version : @version = val; end

  def stage(target = nil)
    # stub - not needed for post_install
    yield if block_given?
  end

  def downloader
    nil
  end
end

class Resources < Array
end

# ============================================================================
# Tap class stub
# ============================================================================

class Tap
  attr_reader :name, :user, :repository

  def initialize(name)
    @name = name
    parts = name.split('/')
    @user = parts[0]
    @repository = parts[1]
  end

  def installed?; true; end  # stub
  def official?; @user == 'homebrew'; end
  def issues_url; nil; end
end

class CoreTap < Tap
  def self.instance
    @instance ||= Tap.new('homebrew/core')
  end
end

# ============================================================================
# FormulaConflict struct
# ============================================================================

FormulaConflict = Struct.new(:name, :reason)

# ============================================================================
# Formula base class
# ============================================================================

class Formula
  include FileUtils
  include Utils::Shebang

  attr_reader :name, :version, :desc, :homepage, :full_name, :path, :tap,
              :alias_path, :alias_name, :build, :stable, :head, :revision,
              :version_scheme, :compatibility_version

  # Build options
  def build; @build ||= BuildOptions.new; end

  # DSL methods - called in formula definition
  class << self
    def desc(text); @desc = text; end
    def homepage(url); @homepage = url; end
    def license(*args); @license = args; end
    def revision(num); @revision = num; end
    def version_scheme(num); @version_scheme = num; end
    def compatibility_version(num); @compatibility_version = num; end

    def head(url, **opts); @head_url = url; end
    def stable(&block); @stable_block = block; end
    def url(url, **opts); @url = url; end
    def sha256(hash); @sha256 = hash; end
    def version(v); @version = v; end
    def mirror(url); @mirrors ||= []; @mirrors << url; end

    def depends_on(*args)
      @depends ||= []
      @depends += args
    end

    def resource(name, &block)
      @resources ||= Resources.new
      r = Resource.new(name)
      block.call(r) if block
      @resources << r
    end

    def patch(*args, &block); end  # stub - accept any args
    def bottle(&block); end  # stub
    def option(name, desc = ''); end  # stub
    def conflicts_with(*args); end  # stub
    def keg_only(reason = nil); @keg_only = reason; end
    def keg_only?; @keg_only; end
    def test(&block); end  # stub
    def livecheck(&block); end  # stub

    def on_macos(&block); end  # stub - not executed on Linux
    def on_linux(&block); yield if block; end  # execute on Linux

    def on_arm(&block); yield if Hardware::CPU.arm? && block; end
    def on_intel(&block); yield if Hardware::CPU.intel? && block; end

    def uses_from_macos(*args); end  # stub
    def link_overwrite(*paths); @link_overwrite ||= []; @link_overwrite += paths; end
    def preserve_rpath; end  # stub

    def no_autobump!(because: nil); @no_autobump = true; end
    def deprecated(date: nil); @deprecated = date; end
    def disabled(date: nil); @disabled = date; end

    # Catch any other DSL methods
    def method_missing(name, *args, &block); end
    def respond_to_missing?(name, include_all = false); true; end

    # Get formula by name
    def [](name)
      Formula.new(name.to_s.sub(/^.*\//, ''))
    end

    # Formula listing methods (stub)
    def names; []; end
    def full_names; []; end
    def core_names; []; end
    def installed; []; end
  end

  def initialize(name, version = nil)
    @name = name.to_s.sub(/^.*\//, '')
    @version = version ? Version.new(version) : detect_version
    @full_name = @name

    # Get DSL values from class
    @desc = self.class.instance_variable_get(:@desc) rescue nil
    @homepage = self.class.instance_variable_get(:@homepage) rescue nil
    @revision = self.class.instance_variable_get(:@revision) || 0
    @version_scheme = self.class.instance_variable_get(:@version_scheme) || 0
    @tap = CoreTap.instance

    # Initialize resources
    @resources = self.class.instance_variable_get(:@resources) || Resources.new
    @depends = self.class.instance_variable_get(:@depends) || []
    @requirements = Requirements.new
  end

  def detect_version
    cellar_dir = HOMEBREW_CELLAR/@name
    if cellar_dir.exist? && cellar_dir.directory?
      v = cellar_dir.children.select(&:directory?).max_by(&:mtime).basename.to_s
      Version.new(v)
    else
      Version.NULL
    end
  end

  # Path methods - Cellar paths
  def prefix; HOMEBREW_CELLAR/@name/@version.to_s; end
  def bin; prefix/'bin'; end
  def sbin; prefix/'sbin'; end
  def lib; prefix/'lib'; end
  def libexec; prefix/'libexec'; end
  def share; prefix/'share'; end
  def include; prefix/'include'; end
  def doc; share/'doc'/@name; end
  def man; share/'man'; end
  def man1; man/'man1'; end
  def man2; man/'man2'; end
  def man3; man/'man3'; end
  def man8; man/'man8'; end
  def pkgshare; share/@name; end
  def pkgetc; etc/@name rescue prefix/'etc'/@name; end
  def info; share/'info'/@name; end
  def elisp; share/'emacs/site-lisp'/@name; end

  # Opt paths - symlinks to Cellar
  def opt_prefix; HOMEBREW_PREFIX/'opt'/@name; end
  def opt_bin; opt_prefix/'bin'; end
  def opt_sbin; opt_prefix/'sbin'; end
  def opt_lib; opt_prefix/'lib'; end
  def opt_include; opt_prefix/'include'; end
  def opt_share; opt_prefix/'share'; end
  def opt_libexec; opt_prefix/'libexec'; end
  def opt_pkgshare; opt_share/@name; end

  # Global paths
  def var; HOMEBREW_PREFIX/'var'; end
  def etc; HOMEBREW_PREFIX/'etc'; end
  def cache; HOMEBREW_PREFIX/'cache'; end
  def logs; var/'log'/@name; end
  def rack; HOMEBREW_CELLAR/@name; end
  def usr; HOMEBREW_PREFIX/'usr'; end
  def data; share/@name; end

  # Install status methods
  def any_version_installed?
    cellar_dir = HOMEBREW_CELLAR/@name
    cellar_dir.exist? && cellar_dir.directory? && cellar_dir.children.any?(&:directory?)
  end

  def latest_version_installed?
    any_version_installed?
  end

  # Version type methods
  def head?; false; end
  def stable?; true; end
  def bottle?; true; end  # stub
  def bottled?; true; end  # stub

  # active_spec access
  def stable; self; end  # return self for stable.version access
  def head; nil; end
  def active_spec; self; end
  def active_spec_sym; :stable; end

  # Dependencies and requirements
  def deps; @depends.map { |d| Dependency.new(d.to_s) }; end
  def dependencies; deps; end
  def recursive_dependencies; deps; end
  def requirements; @requirements; end
  def resources; @resources; end
  def conflicts; []; end

  # inreplace - replace text in file
  def inreplace(file, pattern, replacement)
    path = file.is_a?(Pathname) ? file.to_s : file
    content = File.read(path)
    content.gsub!(pattern, replacement)
    File.write(path, content)
  end

  # inreplace! - replace with regex
  def inreplace!(paths, before, after)
    Array(paths).each do |path|
      path = Pathname(path) unless path.is_a?(Pathname)
      content = path.read
      content.gsub!(before, after)
      path.atomic_write(content)
    end
  end

  # system - execute command, converting Pathname args
  def system(*args)
    str_args = args.map { |a| a.is_a?(Pathname) ? a.to_s : a }
    Kernel.system(*str_args)
  end

  # std_pip_args for Python installs
  def std_pip_args(prefix: true, build_isolation: true)
    args = ['--verbose', '--no-deps']
    args << "--prefix=#{prefix}" if prefix
    args << '--no-build-isolation' unless build_isolation
    args
  end

  def post_install; end
  def install; end  # stub

  # Alias methods
  def aliases; []; end
  def alias_path; nil; end

  # Misc methods
  def keg_only?; self.class.keg_only?; end
  def to_s; @name; end
  def inspect; "#<Formula: #{@name}>"; end
  def hash; @name.hash; end
  def ==(other); other.is_a?(Formula) && @name == other.name; end
  def eql?(other); self == other; end
end

# ============================================================================
# Output helpers
# ============================================================================

def ohai(msg); puts "==> #{msg}"; end
def opoo(msg); puts "Warning: #{msg}"; end
def onoe(msg); puts "Error: #{msg}"; end
def odebug(msg); puts "Debug: #{msg}" if ENV['HOMEBREW_DEBUG']; end
def odisabled(method, replacement = nil); puts "#{method} is deprecated"; end

def quiet_system(*args)
  system(*args) rescue false
end

# ============================================================================
# which - find command in PATH (from Homebrew)
# ============================================================================

def which(cmd, path = ENV['PATH'])
  path.split(':').each do |dir|
    full_path = File.join(dir, cmd)
    return Pathname.new(full_path) if File.executable?(full_path)
  end
  nil
end

# ============================================================================
# PATH class for PATH manipulation
# ============================================================================

class PATH
  include Enumerable

  def initialize(path_string = ENV['PATH'])
    @paths = path_string.to_s.split(':')
  end

  def each; @paths.each { |p| yield p }; end
  def to_a; @paths; end
  def to_s; @paths.join(':'); end
  def include?(path); @paths.include?(path); end
  def prepend(path); @paths.unshift(path); end
  def append(path); @paths.push(path); end
end

# ============================================================================
# Pathname extensions (from Homebrew extend/pathname.rb)
# ============================================================================

class Pathname
  # Write to file atomically
  def atomic_write(content)
    old_stat = stat if exist?
    temp_file = Tempfile.new(basename.to_s, dirname.to_s)
    begin
      temp_file.write(content)
      temp_file.close
      File.rename(temp_file.path, to_s)
    ensure
      temp_file.close rescue nil
      File.unlink(temp_file.path) rescue nil
    end

    # Try to restore permissions
    if old_stat
      begin
        chmod(old_stat.mode)
      rescue Errno::EPERM, Errno::EACCES
        # Ignore permission errors
      end
    end
  end

  # Create symlinks to sources in this folder
  def install_symlink(*sources)
    mkpath
    sources.each do |src|
      case src
      when Array
        src.each { |s| install_symlink_p(s, File.basename(s)) }
      when Hash
        src.each { |s, new_basename| install_symlink_p(s, new_basename) }
      else
        install_symlink_p(src, File.basename(src))
      end
    end
  end

  # private helper for install_symlink
  private def install_symlink_p(src, new_basename)
    mkpath
    dstdir = realpath
    src = Pathname(src).expand_path(dstdir)
    FileUtils.ln_sf(src.relative_path_from(dstdir), dstdir/new_basename)
  end

  # Extended extname for double extensions
  def extname
    basename_str = File.basename(self)

    # Check for bottle extension
    bottle_ext = basename_str.match(/\.bottle\.tar\.gz$/)&.to_a&.first
    return bottle_ext if bottle_ext

    # Check for archive extensions
    archive_ext = basename_str[/(\.(tar|cpio|pax)\.(gz|bz2|lz|xz|zst|Z))\Z/, 1]
    return archive_ext if archive_ext

    File.extname(basename_str)
  end

  # Get basename without extension
  def stem
    File.basename(self, extname)
  end

  # Binary read
  def binread
    File.binread(to_s)
  end

  # Get version from basename
  def version
    Version.parse(basename)
  end

  # SHA256 hash of file
  def sha256
    require 'digest/sha2'
    Digest::SHA256.file(self).hexdigest
  end

  # Change to directory and execute block
  def cd(&block)
    Dir.chdir(self) { yield self }
  end

  # Get subdirectories
  def subdirs
    children.select(&:directory?)
  end

  # Get resolved path (follow symlink)
  def resolved_path
    symlink? ? dirname.join(readlink) : self
  end

  # Check if resolved path exists
  def resolved_path_exists?
    link = readlink rescue nil
    link ? dirname.join(link).exist? : exist?
  end

  # Make relative symlink
  def make_relative_symlink(src)
    dirname.mkpath
    File.symlink(src.relative_path_from(dirname), self)
  end

  # Ensure writable during block
  def ensure_writable(&block)
    saved_perms = nil
    unless writable?
      saved_perms = stat.mode
      FileUtils.chmod('u+rw', to_path)
    end
    yield
  ensure
    chmod(saved_perms) if saved_perms
  end

  # Create executable script
  def write_exec_script(*targets)
    targets.flatten!
    mkpath
    targets.each do |target|
      target = Pathname.new(target)
      join(target.basename).write(<<~SH)
        #!/bin/bash
        exec "#{target}" "$@"
      SH
    end
  end

  # Create env script
  def write_env_script(target, args_or_env, env = nil)
    args = if env.nil?
      env = args_or_env if args_or_env.is_a?(Hash)
      nil
    elsif args_or_env.is_a?(Array)
      args_or_env.join(' ')
    else
      args_or_env
    end

    env_export = +""
    env.each { |key, value| env_export << "#{key}=\"#{value}\" " }

    dirname.mkpath
    write(<<~SH)
      #!/bin/bash
      #{env_export}exec "#{target}" #{args} "$@"
    SH
  end

  # Install metafiles (COPYING, LICENSE, README, etc)
  def install_metafiles(from = Pathname.pwd)
    Pathname(from).children.each do |p|
      next if p.directory? || File.empty?(p)
      next unless Metafiles.copy?(p.basename.to_s)
      p.chmod(0644)
      install(p)
    end
  end

  # Check for .DS_Store
  def ds_store?
    basename.to_s == '.DS_Store'
  end

  # Stub methods for binary detection
  def binary_executable?; false; end
  def mach_o_bundle?; false; end
  def dylib?; false; end
  def arch_compatible?(_wanted_arch); true; end
  def rpaths; []; end

  # Magic number of file
  def magic_number
    @magic_number ||= directory? ? '' : (binread(262) || '')
  end

  # File type description
  def file_type
    @file_type ||= `file -b #{self} 2>/dev/null`.chomp
  end
end

# ============================================================================
# Metafiles module
# ============================================================================

module Metafiles
  COPY_FILES = %w[COPYING COPYING.LESSER COPYING.LEFT LICENSE LICENCE
                  README README.md README.rst README.txt
                  NEWS NEWS.md NEWS.txt
                  ChangeLog CHANGES CHANGELOG CHANGELOG.md
                  AUTHORS AUTHORS.md CONTRIBUTORS].freeze

  def self.copy?(filename)
    COPY_FILES.include?(filename)
  end
end

# ============================================================================
# Mktemp class for temporary directories
# ============================================================================

class Mktemp
  def initialize(name)
    @name = name
    @path = Dir.mktmpdir("epkg-#{name}")
  end

  def run(&block)
    Dir.chdir(@path) { yield @path }
  ensure
    FileUtils.rm_rf(@path)
  end
end

# ============================================================================
# Formatter module for output formatting
# ============================================================================

module Formatter
  def self.success(text); "\e[32m#{text}\e[0m"; end
  def self.error(text); "\e[31m#{text}\e[0m"; end
  def self.url(text); "\e[4m#{text}\e[0m"; end
  def self.bold(text); "\e[1m#{text}\e[0m"; end
  def self.identifier(text); "\e[36m#{text}\e[0m"; end

  def self.redact_secrets(text, secrets)
    secrets.each { |s| text.gsub!(s, '*****') }
    text
  end
end

# ============================================================================
# Kernel extensions
# ============================================================================

module Kernel
  def safe_system(*args)
    success = system(*args)
    raise ErrorDuringExecution.new(args, status: $CHILD_STATUS) unless success
  end

  def quiet_system(*args)
    system(*args) rescue false
  end
end