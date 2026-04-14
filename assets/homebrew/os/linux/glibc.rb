# Stub for Homebrew's os/linux/glibc module
# This is loaded by formulas like glibc.rb that require this module

module OS
  module Linux
    module Glibc
      # Return the system's glibc version (from ldd or libc.so.6)
      def self.system_version
        @system_version ||= begin
          # Get glibc version from ldd output
          ldd_output = `ldd --version 2>/dev/null || /lib/libc.so.6 2>/dev/null`
          if ldd_output =~ /(\d+\.\d+)/
            Version.new($1)
          else
            Version.new("2.0")
          end
        end
      end

      # Return the brewed glibc version (from cellar if installed)
      def self.version
        @version ||= begin
          cellar_glibc = HOMEBREW_CELLAR/'glibc'
          if cellar_glibc.exist? && cellar_glibc.directory?
            v = cellar_glibc.children.select(&:directory?).max_by(&:mtime).basename.to_s
            Version.new(v)
          else
            system_version
          end
        end
      end
    end
  end
end