# epkg_postinstall_runner.rb - Runner script for post_install execution
#
# This script loads the formula stub and formula file, then executes post_install.
# It's executed by epkg with: ruby --disable=gems,rubyopt epkg_postinstall_runner.rb <stub> <formula> <pkgname> <version>
#
# Usage:
#   ruby --disable=gems,rubyopt epkg_postinstall_runner.rb \
#       <stub_path> <formula_path> <pkgname> <version>

stub_path = ARGV[0]
formula_path = ARGV[1]
pkgname = ARGV[2]
version = ARGV[3]

begin
  load stub_path
  load formula_path

  # Find the formula class (last defined class inheriting from Formula)
  formula_class = ObjectSpace.each_object(Class).select { |c| c < Formula && c != Formula }.last

  if formula_class
    formula = formula_class.new(pkgname, version)
    if formula.method(:post_install).owner != Formula
      puts "==> Running post_install for #{pkgname}"
      formula.post_install
      puts "==> post_install completed"
    end
  else
    puts "Warning: No Formula class found"
  end
rescue Exception => e
  puts "Error: #{e.class}: #{e.message}"
  puts e.backtrace.first(5).join("\n")
  exit 1
end