# Stub for Homebrew's os/linux/glibc module
# This is loaded by formulas like glibc.rb that require this module

module OS
  module Linux
    module Glibc
      # Return the system's glibc version (from ldd or libc.so.6)
      def self.system_version
        @system_version ||= begin
          # Get glibc version from ldd output
          version = `ldd --version 2>/dev/null`[/ (\d+\.\d+)/, 1]
          if version
            Version.new(version)
          else
            Version.NULL
          end
        end
      end

      # Return the brewed glibc version (from cellar if installed)
      def self.version
        @version ||= begin
          ldd_path = HOMEBREW_PREFIX/'opt/glibc/bin/ldd'
          if ldd_path.executable?
            version = `#{ldd_path} --version 2>/dev/null`[/ (\d+\.\d+)/, 1]
            if version
              Version.new(version)
            else
              system_version
            end
          else
            system_version
          end
        end
      end

      # Minimum supported glibc version
      def self.minimum_version
        Version.new('2.17')
      end

      def self.below_minimum_version?
        system_version < minimum_version
      end

      def self.below_ci_version?
        false  # stub
      end
    end
  end
end