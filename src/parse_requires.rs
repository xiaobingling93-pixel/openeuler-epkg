use lazy_static::lazy_static;
use regex::Regex;
use std::fmt;
use std::error::Error;
use url::Url;

/*
 * Design and Rules for Parsing Package Requirements
 *
 * This implementation normalizes package requirements from various package managers
 * (RPM, DEB, Arch Linux, Python, Conda) into a consistent hierarchical structure.
 * The goal is to represent dependencies in a unified way, making it easier to
 * compare, analyze, or process them programmatically.
 *
 * ### Normalized Data Structure
 *
 * The output is structured as follows:
 *
 * 1. `and_depends`: A list of `or_depends` (logical AND groups).
 *    - Each `or_depends` represents a group of dependencies that must be satisfied together.
 *    - and_depends := [or_depends1, or_depends2, or_depends3, ...]
 *
 * 2. `or_depends`: A list of `pkg_depend` (logical OR groups).
 *    - Each `pkg_depend` represents an alternative dependency that can satisfy the requirement.
 *    - or_depends := [pkg_depend1, pkg_depend2, ...]
 *
 * 3. `pkg_depend`: A tuple of `capability` and `constraints`.
 *    - `capability`: A string representing the package name, filename, or module name.
 *      - capability := String of pkgname | filename | modulename | ...
 *    - `constraints`: A list of `version_constraint` (version or conditional requirements).
 *      - pkg_depend := (capability, [version_constraint1, version_constraint2, ...])
 *
 * 4. `version_constraint`: A tuple of `operator` and `operand`.
 *    - `operator`: An enum representing the relationship (e.g., `VersionGreaterThan`, `VersionGreaterThanEqual`, `IfInstall`).
 *      - operator := Enum {
 *          IfInstall,                     // Dependency is required if the operand capability is installed.
 *          VersionGreaterThan,            // Version must be greater than the operand.
 *          VersionGreaterThanEqual,       // Version must be greater than or equal to the operand.
 *          VersionEqual,                  // Version must be equal to the operand.
 *          VersionLessThan,               // Version must be less than the operand.
 *          VersionLessThanEqual,          // Version must be less than or equal to the operand.
 *          VersionNotEqual,               // Version must not be equal to the operand.
 *          ...
 *      }
 *    - `operand`: A string representing the version or condition.
 *      - operand := String of version | capability (when operator=IfInstall)
 *
 * ### Rules for Parsing
 *
 * 1. **Logical Operators**:
 *    - `and`: Represents a conjunction of dependencies (all must be satisfied).
 *    - `or`: Represents a disjunction of dependencies (any one can satisfy the requirement).
 *    - `if`: Represents a conditional dependency (e.g., "feh if Xserver").
 *
 * 2. **Version Constraints**:
 *    - Operators: `>=`, `<=`, `>`, `<`, `=`, `==`, `!=`.
 *    - Operands: Version strings or capabilities (for `if` conditions).
 *
 * 3. **Parentheses Handling**:
 *    - Nested parentheses are supported (e.g., `(feh and xrandr) if Xserver`).
 *    - Parentheses are used to group logical expressions.
 *
 * 4. **Package-Specific Rules**:
 *    - **RPM**:
 *      - Supports conditional dependencies (`if`).
 *      - Handles nested logical expressions.
 *    - **DEB**:
 *      - Uses `|` for alternatives (logical OR).
 *      - Version constraints are enclosed in parentheses (e.g., `libc6 (>= 2.34)`).
 *    - **Arch Linux**:
 *      - Simple space-separated dependencies.
 *      - Version constraints are directly appended to package names.
 *    - **Python**:
 *      - Supports environment markers (e.g., `; sys.platform == "win32"`).
 *      - Multiple constraints per package (e.g., `pbr!=2.1.0,>=2.0.0`).
 *    - **Conda**:
 *      - Simple space-separated dependencies.
 *      - Version constraints use `>=`, `<=`, `=`, etc.
 *
 * ### Error Handling
 *
 * 1. **Unbalanced Parentheses**:
 *    - Detects and reports mismatched parentheses.
 * 2. **Invalid Format**:
 *    - Reports malformed package requirements (e.g., missing version after operator).
 * 3. **Unsupported Operator**:
 *    - Reports unrecognized operators.
 * 4. **Unsupported Package Type**:
 *    - Reports if the package type is not supported.
 *
 * ### Example Inputs and Outputs
 *
 * 1. **RPM Input**: `"((feh and xrandr) if Xserver)"`
 *    - Output:
 *      [
 *        [
 *          PkgDepend { capability: "feh", constraints: [VersionConstraint { operator: IfInstall, operand: "Xserver" }] },
 *        ],
 *        [
 *          PkgDepend { capability: "xrandr", constraints: [VersionConstraint { operator: IfInstall, operand: "Xserver" }] },
 *        ],
 *      ]
 *
 * 2. **DEB Input**: `"libc6 (>= 2.34), libgcc-s1 (>= 3.0) | gcc"`
 *    - Output:
 *      [
 *        [
 *          PkgDepend { capability: "libc6", constraints: [VersionConstraint { operator: VersionGreaterThanEqual, operand: "2.34" }] },
 *        ],
 *        [
 *          PkgDepend { capability: "libgcc-s1", constraints: [VersionConstraint { operator: VersionGreaterThanEqual, operand: "3.0" }] },
 *          PkgDepend { capability: "gcc", constraints: [] },
 *        ],
 *      ]
 *
 * 3. **Python Input**: `"networkx>=2.3.0\npbr!=2.1.0,>=2.0.0"`
 *    - Output:
 *      [
 *        [
 *          PkgDepend { capability: "networkx", constraints: [VersionConstraint { operator: VersionGreaterThanEqual, operand: "2.3.0" }] },
 *        ],
 *        [
 *          PkgDepend { capability: "pbr", constraints: [
 *            VersionConstraint { operator: VersionNotEqual, operand: "2.1.0" },
 *            VersionConstraint { operator: VersionGreaterThanEqual, operand: "2.0.0" },
 *          ]},
 *        ],
 *      ]
 *
 * 4. **Conda Input**: `"python 3.6*, cudatoolkit 9.0.*"`
 *    - Output:
 *      [
 *        [
 *          PkgDepend { capability: "python", constraints: [VersionConstraint { operator: VersionEqual, operand: "3.6*" }] },
 *        ],
 *        [
 *          PkgDepend { capability: "cudatoolkit", constraints: [VersionConstraint { operator: VersionEqual, operand: "9.0.*" }] },
 *        ],
 *      ]
 *
 * ### Extensibility
 *
 * - New package types can be added by implementing their respective parsers.
 * - The normalized structure (`AndDepends`) ensures consistency across package types.
 */

#[derive(Debug, PartialEq, Eq, Clone)]
#[allow(dead_code)]
pub enum Operator {
    IfInstall,
    VersionGreaterThan,
    VersionGreaterThanEqual,
    VersionLessThan,
    VersionLessThanEqual,
    VersionEqual,
    VersionNotEqual,
    VersionCompatible,
    VersionEqualStar,
    VersionNotEqualStar,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct VersionConstraint {
    pub operator: Operator,
    pub operand: String,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PkgDepend {
    pub capability: String,
    pub constraints: Vec<VersionConstraint>,
}

pub type OrDepends = Vec<PkgDepend>;
pub type AndDepends = Vec<OrDepends>;

#[derive(Debug, PartialEq)]
pub enum ParseError {
    UnbalancedParentheses,
    InvalidFormat(String),
    UnsupportedOperator,
    UnsupportedPackageType,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ParseError::UnbalancedParentheses => write!(f, "Unbalanced parentheses"),
            ParseError::InvalidFormat(msg) => write!(f, "Invalid format: {}", msg),
            ParseError::UnsupportedOperator => write!(f, "Unsupported operator"),
            ParseError::UnsupportedPackageType => write!(f, "Unsupported package type"),
        }
    }
}

impl Error for ParseError {}

/// Parses the `requires` field into a normalized `AndDepends` structure.
pub fn parse_requires(package_type: &str, requires: &str) -> Result<AndDepends, ParseError> {
    match package_type {
        "rpm" => parse_rpm_requires(requires),
        "deb" => parse_deb_requires(requires),
        "archlinux" => parse_archlinux_requires(requires),
        "python" => parse_python_requires(requires),
        "conda" => parse_conda_requires(requires),
        _ => Err(ParseError::UnsupportedPackageType),
    }
}

lazy_static! {
    static ref OPERATOR_REGEX: Regex = Regex::new(r"(>=|<=|==|!=|>|<|=|~=)").unwrap();
    static ref ARCHLINUX_COMMENT_REGEX: Regex = Regex::new(r"\s*: .*").unwrap();
    static ref PYTHON_COMMENT_REGEX: Regex = Regex::new(r"\s*# .*").unwrap();
}

/// Parses RPM-style requirements.
// Case1: single depend
// Requires: lua < 5.2
// Requires: lua = 5.2
// Requires: lua <= 5.2
// Requires: pixman >= 0.30.0
// Requires: perl(Net::LibIDN)
// Requires: perl(Net::SSLeay)
// Requires: perl(Net::Server) >= 2.0
//
// Case2: several AND depends separated by ','
// Requires: xfsprogs >= 2.6.30, attr >= 2.0.0
// Requires: pam >= 1.1.3-7, /etc/pam.d/system-auth
//
// Case3: (A or B or C)
// Requires: (containerd or cri-o or docker or docker-ce or moby-engine)
// Requires: (glibc-langpack-en or glibc-all-langpacks)
// Requires: (mysql or mariadb)
// Requires: (wget or curl)
// Requires: (NetworkManager >= 1.20 or dhclient)
// Requires: (util-linux-core or util-linux)
// Requires: (wpa_supplicant >= 1.1 or iwd)
//
// Case4: (A if B)
// Requires: (libsss_sudo if sudo)
// Requires: (python3dist(ovs) if openvswitch)
//
// Case5: ((A and B) if C)
// Requires: ((feh and xrandr) if Xserver)
//
// Case6: (A if (B or C))
// Requires: (syslinux if (filesystem.x86_64 or filesystem.i686))
// Requires(post): ((policycoreutils-python-utils and libselinux-utils) if (selinux-policy-targeted or selinux-policy-mls))
pub fn parse_rpm_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let requires = requires.trim();

    // Step 1: Remove surrounding parentheses and recurse only if the entire string is enclosed
    if has_outer_parentheses(requires)? {
        let inner = &requires[1..(requires.len() - 1)];
        // println!("dive into {:#?}", inner);
        return parse_rpm_requires(inner);
    }

    // Step 2: Split into capability and condition parts if " if " is present
    // input: ((A and B and C) if (X or Y))
    // =>
    // output: [[A if X; A if Y], [B if X; B if Y], [C if X; C if Y]]
    // so
    // - the final and_depends.size == capability_deps.size
    // - each pkg_depend.size *= condition_or.size
    if let Some((capability_part, condition_part)) = requires.split_once(" if ") {
        let capability_part = capability_part.replace(" and ", ",");
        let capability_deps = parse_rpm_requires(&capability_part)?;
        let condition_deps = parse_rpm_requires(condition_part)?;

        let mut and_depends = Vec::new();

        for capability_and in capability_deps {
            let mut combined_or = Vec::new();
            for pkg_depend in capability_and {
                // For each condition in condition_deps (OR clauses), generate combinations
                for condition_or in condition_deps.iter().flatten() {
                    let mut new_constraints = pkg_depend.constraints.clone();
                    new_constraints.push(VersionConstraint {
                        operator: Operator::IfInstall,
                        operand: condition_or.capability.clone(),
                    });
                    combined_or.push(PkgDepend {
                        capability: pkg_depend.capability.clone(),
                        constraints: new_constraints,
                    });
                }
            }
            if !combined_or.is_empty() {
                and_depends.push(combined_or);
            }
        }

        return Ok(and_depends);
    }

    // Step 3: Split AND clauses by commas
    let mut and_depends = Vec::new();
    for and_clause in requires.split(',') {
        let and_clause = and_clause.trim();
        if and_clause.is_empty() {
            continue;
        }

        // Step 4: Split OR clauses by " or " and parse each
        let or_clauses = and_clause.split(" or ");

        let mut or_depends = Vec::new();
        for or_clause in or_clauses {
            let (name, constraints) = parse_package(&or_clause)?;
            or_depends.push(PkgDepend {
                capability: name,
                constraints,
            });
        }

        if !or_depends.is_empty() {
            and_depends.push(or_depends);
        }
    }

    Ok(and_depends)
}

/// Checks if the entire string is enclosed in a pair of balanced parentheses.
/// Returns
/// - `Ok(true)` if fully enclosed
/// - `Ok(false)` if not
/// - `Err(ParseError::UnbalancedParentheses)` if the parentheses are unbalanced.
/// We only check UnbalancedParentheses BTW, so won't detect all errors.
// Detects pairing outer ( ) in
// - "(A and B and C)"
// - "(A or B or C)"
// - "((A and B and C) if (X or Y))"
// But skip this seemingly leading/trailing ( ):
// - "(A and B and C) if (X or Y)"
fn has_outer_parentheses(s: &str) -> Result<bool, ParseError> {
    let s = s.trim();

    // Early return if the string does not start with '('
    if !s.starts_with('(') {
        return Ok(false);
    }

    let mut depth = 0;
    for (i, c) in s.chars().enumerate() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                // If depth goes negative, parentheses are unbalanced
                if depth < 0 {
                    return Err(ParseError::UnbalancedParentheses);
                }
            }
            _ => {}
        }

        // If depth drops to 0 before the end, the outermost parentheses are not enclosing the entire string
        if depth == 0 && i != s.len() - 1 {
            return Ok(false);
        }
    }

    // If depth is not 0 at the end, parentheses are unbalanced
    if depth != 0 {
        return Err(ParseError::UnbalancedParentheses);
    }

    // If we reach here, the string is fully enclosed in balanced parentheses
    Ok(true)
}

/// Parses a package clause into a name and version constraints.
fn parse_package(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    // Normalize the clause by adding whitespace around operators
    let normalized_clause = OPERATOR_REGEX.replace_all(clause, " $1 ").to_string();
    let mut parts = normalized_clause.split_whitespace();

    let name = parts.next().ok_or(ParseError::InvalidFormat("Missing package name".to_string()))?.to_string();
    let mut constraints = Vec::new();
    let mut current_operator = None;

    for part in parts {
        if let Some(op) = parse_operator(part) {
            current_operator = Some(op);
        } else if let Some(op) = current_operator.take() {
            constraints.push(VersionConstraint {
                operator: op,
                operand: part.to_string(),
            });
        } else {
            println!("parse_package error, invalid operator '{}' in clause {}", part, normalized_clause);
            return Err(ParseError::UnsupportedOperator);
        }
    }

    Ok((name, constraints))
}

/// Parses DEB-style requirements.
// Example inputs:
// Depends: aria2 | wget | curl, binutils, wine
// Depends: libc6 (>= 2.34)
// Depends: ruby-activesupport (>= 2:5.2.0), ruby-activesupport (<< 2:8.0), ruby-concurrent (>= 1.0), ruby-method-source (>= 1.0)
// Depends: librust-bincode-1+default-dev (>= 1.3.3-~~), librust-jargon-args-0.2+default-dev (>= 0.2.5-~~)
// Depends: libc6 (>= 2.34), libgcc-s1 (>= 4.2)
// Depends: debconf (>= 0.5) | debconf-2.0, wget
// Depends: xwayland, libxcursor1 (>> 1.1.2)
// Depends: python3:any (>= 3.11), python3-magcode-core (>= 1.5.4~), python3-setproctitle
fn parse_deb_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let mut and_depends = Vec::new();

    // Split the input into individual dependencies using commas
    for dependency in requires.split(',') {
        let dependency = dependency.trim();
        if dependency.is_empty() {
            continue;
        }

        // Split the dependency into alternatives using the `|` operator
        let mut or_depends = Vec::new();
        for alternative in dependency.split('|') {
            let alternative = alternative.trim();
            if alternative.is_empty() {
                continue;
            }

            // Parse each alternative as a separate `PkgDepend`
            let (name, constraints) = parse_debian_package(alternative)?;
            or_depends.push(PkgDepend {
                capability: name,
                constraints,
            });
        }

        and_depends.push(or_depends);
    }

    Ok(and_depends)
}

/// Parses a DEB-style package requirement into a name and version constraints.
fn parse_debian_package(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    let clause = clause.trim();

    // Check if the clause contains version constraints in parentheses
    if let Some((name, version_part)) = clause.split_once('(') {
        let name = name.trim().to_string();

        // Extract the version constraints from inside the parentheses
        let version_part = version_part.trim();
        if !version_part.ends_with(')') {
            return Err(ParseError::InvalidFormat("Missing closing parenthesis".to_string()));
        }

        let version_part = &version_part[..version_part.len() - 1]; // Remove the closing ')'
        let constraints = parse_version_constraints(version_part)?;

        Ok((name, constraints))
    } else {
        // No version constraints, just the package name
        Ok((clause.to_string(), Vec::new()))
    }
}

/// Parses version constraints from a string
/// Example inputs:
/// - ">= 2.34" or ">= 2.34, << 3.0"
/// - ">=1.14.12,<2.0a0"
fn parse_version_constraints(version_part: &str) -> Result<Vec<VersionConstraint>, ParseError> {
    let mut constraints = Vec::new();

    // Split the version part by commas to handle multiple constraints
    for constraint in version_part.split(',') {
        let constraint = constraint.trim();
        if constraint.is_empty() {
            continue;
        }

        let (mut operator, op_len) = match parse_operator_from_start(constraint) {
            Some((op, len)) => (op, len),
            None => (Operator::VersionEqual, 0), // Conda case: if no operator is found, assume it's a version constraint with "=="
        };

        let operand = if op_len > 0 {
            constraint[op_len..].trim().to_string()
        } else {
            constraint.to_string()
        };

        if operand.is_empty() {
            return Err(ParseError::InvalidFormat(format!(
                "Invalid version constraint: {}",
                constraint
            )));
        }

        // Check if the operand ends with `.*` and update the operator accordingly
        if operand.ends_with(".*") {
            if operator == Operator::VersionEqual {
                operator = Operator::VersionEqualStar;
            } else if operator == Operator::VersionNotEqual {
                operator = Operator::VersionNotEqualStar;
            }
        };

        constraints.push(VersionConstraint { operator, operand });
    }

    Ok(constraints)
}

/// Parses an operator from the start of a string (e.g., ">=1.14.12" -> (Operator::VersionGreaterThanEqual, 2)).
fn parse_operator_from_start(s: &str) -> Option<(Operator, usize)> {
    if s.starts_with(">=") {
        Some((Operator::VersionGreaterThanEqual, 2))
    } else if s.starts_with("<=") {
        Some((Operator::VersionLessThanEqual, 2))
    } else if s.starts_with(">") {
        Some((Operator::VersionGreaterThan, 1))
    } else if s.starts_with(">>") {
        Some((Operator::VersionGreaterThan, 2))
    } else if s.starts_with("<") {
        Some((Operator::VersionLessThan, 1))
    } else if s.starts_with("<<") {
        Some((Operator::VersionLessThan, 2))
    } else if s.starts_with("=") {
        Some((Operator::VersionEqual, 1))
    } else if s.starts_with("==") {
        Some((Operator::VersionEqual, 2))
    } else if s.starts_with("!=") {
        Some((Operator::VersionNotEqual, 2))
    } else if s.starts_with("~=") {
        Some((Operator::VersionCompatible, 2))
    } else {
        None
    }
}

/// Parses an operator string into an `Operator` enum.
fn parse_operator(op: &str) -> Option<Operator> {
    match op {
        ">=" => Some(Operator::VersionGreaterThanEqual),
        "<=" => Some(Operator::VersionLessThanEqual),
        ">" | ">>" => Some(Operator::VersionGreaterThan),
        "<" | "<<" => Some(Operator::VersionLessThan),
        "=" | "==" => Some(Operator::VersionEqual),
        "!=" => Some(Operator::VersionNotEqual),
        "~=" => Some(Operator::VersionCompatible),
        "=*" => Some(Operator::VersionEqualStar),    // for use by unit test
        "!*" => Some(Operator::VersionNotEqualStar), // for use by unit test
        "if" => Some(Operator::IfInstall),
        _ => None,
    }
}

/// Parses Arch Linux PKGBUILD requirements.
// Example inputs:
// optdepends=('python-pygments: for syntax highlighting')
// depends=('zsh')
// makedepends=('asciidoc')
// depends=('zsh>=4.3.9')
pub fn parse_archlinux_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let requires = ARCHLINUX_COMMENT_REGEX.replace(requires, "").to_string();
    let mut and_depends = Vec::new();

    for clause in requires.split_whitespace() {
        let clause = clause.trim();
        let (name, constraints) = parse_package(clause)?;
        and_depends.push(vec![PkgDepend {
            capability: name,
            constraints,
        }]);
    }

    Ok(and_depends)
}

/// Parses Python-style requirements.
// Example inputs:
// pbr!=2.1.0,>=2.0.0 # Apache-2.0
// PyYAML>=3.12 # MIT
// flake8<6.0.0,>=3.6.0 # MIT
// jsonschema>=3.0.2 # MIT
// netifaces==0.11.0; sys.platform == "win32"
// ./granulate-utils/
// humanfriendly==10.0
// beautifulsoup4==4.11.1
pub fn parse_python_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let mut and_depends = Vec::new();
    let requires = PYTHON_COMMENT_REGEX.replace(requires, "").to_string();

    for line in requires.lines() {
        let line = line.trim();

        if line.is_empty() {
            continue;
        }

        // Split the line into the spec part and the environment marker (if any)
        let (spec_part, marker) = match line.split_once(';') {
            Some((s, m)) => (s.trim(), Some(m.trim())),
            None => (line, None),
        };

        // Parse the spec part into a package name and version constraints
        let (name, constraints) = parse_python_package(spec_part)?;

        // Add the package to the OR dependencies
        let mut or_depends = Vec::new();
        if let Some(marker) = marker {
            // If there's an environment marker, add it as a conditional constraint
            or_depends.push(PkgDepend {
                capability: name,
                constraints: vec![VersionConstraint {
                    operator: Operator::IfInstall,
                    operand: marker.to_string(),
                }],
            });
        } else {
            // Otherwise, add the package with its version constraints
            or_depends.push(PkgDepend {
                capability: name,
                constraints,
            });
        }

        // Add the OR dependencies to the AND dependencies
        and_depends.push(or_depends);
    }

    Ok(and_depends)
}

/// Parses a Python-style package requirement into a name and version constraints.
fn parse_python_package(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    let clause = clause.trim();

    let (name, version_part) = if let Some(idx) = clause.find(|c: char| c == '>' || c == '<' || c == '=' || c == '!' || c == '~') {
        let name = clause[..idx].trim().to_string();
        let version_part = clause[idx..].trim();
        (name, version_part)
    } else {
        // No version constraints, just the package name
        (clause.to_string(), "")
    };

    // Parse version constraints if present
    let constraints = if !version_part.is_empty() {
        parse_version_constraints(version_part)?
    } else {
        Vec::new()
    };

    Ok((name, constraints))
}

/// Parses Conda-style requirements.
// Example inputs:
// "bzip2",
// "bzip2 >=1.0.6,<2.0a0",
// "cairo 1.14.*",
// "blas 1.0 openblas",
// "r-xgboost 0.72 r343hcdcee97_0"
// "_r-xgboost-mutex 1.0 gpu_0",
// "_r-xgboost-mutex 2.0 cpu_0",
// "_r-mutex 1.* anacondar_1",
// "_r-mutex 1.* mro_2",
pub fn parse_conda_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let mut and_depends = Vec::new();

    // Split the input into parts using whitespace
    let parts: Vec<&str> = requires.split_whitespace().collect();

    // The first part is the package name
    let name = parts.get(0).ok_or(ParseError::InvalidFormat("Missing package name".to_string()))?;

    // The remaining parts are version constraints or additional information
    let constraints = if parts.len() > 1 {
        parse_version_constraints(&parts[1])?
    } else {
        Vec::new()
    };

    // Add the package to the AND dependencies
    and_depends.push(vec![PkgDepend {
        capability: name.to_string(),
        constraints,
    }]);

    Ok(and_depends)
}

pub fn get_package_format(origin_url: &str) -> Option<String> {
    let parsed_url = Url::parse(origin_url).ok()?;
    let path = parsed_url.path();
    let filename = path.split('/').last()?;
    let ext = filename.split('.').last()?;
    Some(ext.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function to create a PkgDepend
    fn pkg(name: &str, constraints: &[(&str, &str)]) -> PkgDepend {
        PkgDepend {
            capability: name.to_string(),
            constraints: constraints
                .iter()
                .map(|(op, ver)| VersionConstraint {
                    operator: parse_operator(op).unwrap(),
                    operand: ver.to_string(),
                })
                .collect(),
        }
    }

    // Helper function to create a PkgDepend with an "if" constraint
    fn pkg_if(name: &str, condition: &str) -> PkgDepend {
        PkgDepend {
            capability: name.to_string(),
            constraints: vec![VersionConstraint {
                operator: Operator::IfInstall,
                operand: condition.to_string(),
            }],
        }
    }

    // Test RPM parsing
    #[test]
    fn test_rpm() {
        // Simple package
        assert_eq!(
            parse_requires("rpm", "pixman").unwrap(),
            vec![vec![pkg("pixman", &[])]]
        );

        // Version constraint
        assert_eq!(
            parse_requires("rpm", "pixman >= 0.30.0").unwrap(),
            vec![vec![pkg("pixman", &[(">=", "0.30.0")])]]
        );

        // File dependency
        assert_eq!(
            parse_requires("rpm", "/etc/pam.d/system-auth").unwrap(),
            vec![vec![pkg("/etc/pam.d/system-auth", &[])]]
        );

        // Logical OR
        assert_eq!(
            parse_requires("rpm", "(mysql or mariadb)").unwrap(),
            vec![vec![
                pkg("mysql", &[]),
                pkg("mariadb", &[])
            ]]
        );

        // Conditional (if)
        assert_eq!(
            parse_requires("rpm", "feh if Xserver").unwrap(),
            vec![vec![pkg("feh", &[("if", "Xserver")])]]
        );

        // Nested conditionals
        assert_eq!(
            parse_requires("rpm", "((feh and xrandr) if Xserver)").unwrap(),
            vec![
                vec![pkg("feh", &[("if", "Xserver")])],
                vec![pkg("xrandr", &[("if", "Xserver")])]
            ]
        );

        // Multiple constraints
        assert_eq!(
            parse_requires("rpm", "perl(Net::Server) >= 2.0 < 3.0").unwrap(),
            vec![vec![pkg("perl(Net::Server)", &[(">=", "2.0"), ("<", "3.0")])]]
        );

        let input = "(containerd or cri-o or docker or docker-ce or moby-engine)";
        let result = parse_requires("rpm", input).unwrap();

        // There should be only 1 AND clause
        assert_eq!(result.len(), 1);

        // The AND clause should contain all OR dependencies
        let or_depends = &result[0];
        assert_eq!(or_depends.len(), 5);
        assert!(or_depends.contains(&pkg("containerd", &[])));
        assert!(or_depends.contains(&pkg("cri-o", &[])));
        assert!(or_depends.contains(&pkg("docker", &[])));
        assert!(or_depends.contains(&pkg("docker-ce", &[])));
        assert!(or_depends.contains(&pkg("moby-engine", &[])));
    }

    // Test the "if" operator with or conditions
    #[test]
    fn test_if_or_conditions() {
        let input = "(A if (B or C))";
        let result = parse_requires("rpm", input).unwrap();

        // Expected structure:
        // [
        //   [A if B, A if C],
        // ]
        assert_eq!(result.len(), 1);
        let first_and = &result[0];
        assert_eq!(first_and.len(), 2);
        assert!(first_and.contains(&pkg_if("A", "B")));
        assert!(first_and.contains(&pkg_if("A", "C")));
    }

    #[test]
    fn test_rpm_if_condition() {
        let input = "((A and B and C) if (X or Y))";
        let result = parse_requires("rpm", input).unwrap();

        // Expected structure:
        // [
        //   [A if X, A if Y],
        //   [B if X, B if Y],
        //   [C if X, C if Y],
        // ]
        assert_eq!(result.len(), 3);

        // Check the first AND clause: [A if X, A if Y]
        let first_and = &result[0];
        println!("{:#?}", first_and);
        assert_eq!(first_and.len(), 2);
        assert!(first_and.contains(&pkg_if("A", "X")));
        assert!(first_and.contains(&pkg_if("A", "Y")));

        // Check the second AND clause: [B if X, B if Y]
        let second_and = &result[1];
        assert_eq!(second_and.len(), 2);
        assert!(second_and.contains(&pkg_if("B", "X")));
        assert!(second_and.contains(&pkg_if("B", "Y")));

        // Check the third AND clause: [C if X, C if Y]
        let third_and = &result[2];
        assert_eq!(third_and.len(), 2);
        assert!(third_and.contains(&pkg_if("C", "X")));
        assert!(third_and.contains(&pkg_if("C", "Y")));
    }

    // Test DEB parsing
    #[test]
    fn test_deb() {
        // Simple package with version
        assert_eq!(
            parse_requires("deb", "libc6 (>= 2.34)").unwrap(),
            vec![vec![pkg("libc6", &[(">=", "2.34")])]]
        );

        // Alternative dependencies
        assert_eq!(
            parse_requires("deb", "libgcc-s1 (>= 3.0) | gcc").unwrap(),
            vec![vec![
                pkg("libgcc-s1", &[(">=", "3.0")]),
                pkg("gcc", &[]),
            ]]
        );

        // Multiple alternatives
        assert_eq!(
            parse_requires("deb", "emacs | emacs-gtk | emacs-lucid").unwrap(),
            vec![vec![
                pkg("emacs", &[]),
                pkg("emacs-gtk", &[]),
                pkg("emacs-lucid", &[]),
            ]]
        );

        // Complex example from original question
        let input = "libao4 (>= 1.1.0), libc6 (>= 2.34), debconf (>= 0.5) | debconf-2.0";
        let result = parse_requires("deb", input).unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains(&vec![pkg("libao4", &[(">=", "1.1.0")])]));
        assert!(result.contains(&vec![pkg("libc6", &[(">=", "2.34")])]));
        assert!(result.contains(&vec![
            pkg("debconf", &[(">=", "0.5")]),
            pkg("debconf-2.0", &[]),
        ]));
    }

    // Test Arch Linux parsing
    #[test]
    fn test_archlinux() {
        // Simple package
        assert_eq!(
            parse_requires("archlinux", "bash: GNU Bourne Again SHell").unwrap(),
            vec![vec![pkg("bash", &[])]]
        );

        // Version constraint
        assert_eq!(
            parse_requires("archlinux", "zsh>=4.3.9").unwrap(),
            vec![vec![pkg("zsh", &[(">=", "4.3.9")])]]
        );

        // Multiple packages
        assert_eq!(
            parse_requires("archlinux", "git python").unwrap(),
            vec![
                vec![pkg("git", &[])],
                vec![pkg("python", &[])]
            ]
        );

        // Complex example from original question
        let input = "python python-gobject ttf-font gtk3 python-xdg";
        let result = parse_requires("archlinux", input).unwrap();
        assert_eq!(result.len(), 5);
        assert!(result.contains(&vec![pkg("python", &[])]));
        assert!(result.contains(&vec![pkg("python-gobject", &[])]));
        assert!(result.contains(&vec![pkg("ttf-font", &[])]));
        assert!(result.contains(&vec![pkg("gtk3", &[])]));
        assert!(result.contains(&vec![pkg("python-xdg", &[])]));
    }

    // Test Python parsing
    #[test]
    fn test_python() {
        // Simple requirement
        assert_eq!(
            parse_requires("python", "networkx>=2.3.0").unwrap(),
            vec![vec![pkg("networkx", &[(">=", "2.3.0")])]]
        );

        // Multiple constraints
        assert_eq!(
            parse_requires("python", "pbr!=2.1.0,>=2.0.0").unwrap(),
            vec![vec![pkg("pbr", &[("!=", "2.1.0"), (">=", "2.0.0")])]]
        );

        // Comment line
        assert_eq!(
            parse_requires("python", "pkg # comment").unwrap(),
            vec![vec![pkg("pkg", &[])]]
        );

        // File path
        assert_eq!(
            parse_requires("python", "./granulate-utils/").unwrap(),
            vec![vec![pkg("./granulate-utils/", &[])]]
        );

        // compatibility operator (~=)
        assert_eq!(
            parse_requires("python", "package~=1.0").unwrap(),
            vec![vec![pkg("package", &[("~=", "1.0")])]]
        );
    }

    // Test Conda parsing
    #[test]
    fn test_conda() {
        // Simple package
        assert_eq!(
            parse_requires("conda", "bwidget").unwrap(),
            vec![vec![pkg("bwidget", &[])]]
        );

        // Version constraint
        assert_eq!(
            parse_requires("conda", "cairo >=1.14.12,<2.0a0").unwrap(),
            vec![vec![pkg("cairo", &[(">=", "1.14.12"), ("<", "2.0a0")])]]
        );

        // Package with version constraints
        let input = "cairo 1.14.*";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("cairo", &[("=*", "1.14.*")])]]
        );

        // Package with multiple version constraints
        let input = "blas 1.0 openblas";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("blas", &[("==", "1.0")])],
            ]
        );

    }

    // Test error handling
    #[test]
    fn test_errors() {
        // Unbalanced parentheses
        assert!(matches!(
            parse_requires("rpm", "(feh and xrandr").unwrap_err(),
            ParseError::UnbalancedParentheses
        ));

        // Unsupported package type
        assert!(matches!(
            parse_requires("npm", "express").unwrap_err(),
            ParseError::UnsupportedPackageType
        ));

        // Empty input
        assert_eq!(
            parse_requires("rpm", "").unwrap(),
            Vec::<OrDepends>::new()
        );
    }

    // Test edge cases
    #[test]
    fn test_edge_cases() {
        // Multiple spaces
        assert_eq!(
            parse_requires("rpm", "  pkg   >=   2.0  ").unwrap(),
            vec![vec![pkg("pkg", &[(">=", "2.0")])]]
        );

    }

    // Test handling of whitespace variations
    #[test]
    fn test_whitespace_variations() {
        // Multiple spaces between package and version
        assert_eq!(
            parse_requires("rpm", "package  >=  1.0").unwrap(),
            vec![vec![pkg("package", &[(">=", "1.0")])]]
        );

        // No spaces between package and version
        assert_eq!(
            parse_requires("rpm", "package>=1.0").unwrap(),
            vec![vec![pkg("package", &[(">=", "1.0")])]]
        );
    }

    // Test parsing of package types with different naming conventions
    #[test]
    fn test_package_naming_conventions() {
        // RPM with namespace
        assert_eq!(
            parse_requires("rpm", "perl(Net::LibIDN)").unwrap(),
            vec![vec![pkg("perl(Net::LibIDN)", &[])]]
        );

        // DEB with colon in name
        assert_eq!(
            parse_requires("deb", "lib:package").unwrap(),
            vec![vec![pkg("lib:package", &[])]]
        );

        // Python with hyphen in name
        assert_eq!(
            parse_requires("python", "package-name").unwrap(),
            vec![vec![pkg("package-name", &[])]]
        );
    }

    #[test]
    fn test_parentheses() {
        // Case 1: Fully enclosed and balanced
        let input = "(A and B and C)";
        assert_eq!(has_outer_parentheses(input), Ok(true));

        // Case 2: Not fully enclosed
        let input = "(A and B and C) if (X or Y)";
        assert_eq!(has_outer_parentheses(input), Ok(false));

        // Case 3: Unbalanced parentheses (missing closing parenthesis)
        let input = "(A and B and C";
        assert_eq!(
            has_outer_parentheses(input),
            Err(ParseError::UnbalancedParentheses)
        );

        // Case 4: Unbalanced parentheses (missing opening parenthesis)
        let input = "(A and B and C))";
        assert_eq!(
            has_outer_parentheses(input),
            Ok(false)
        );

        // Case 5: Unbalanced parentheses (nested and missing closing parenthesis)
        let input = "(A and (B and C)";
        assert_eq!(
            has_outer_parentheses(input),
            Err(ParseError::UnbalancedParentheses)
        );

        // Case 6: Nested and balanced
        let input = "((A and B) if (X or Y))";
        assert_eq!(has_outer_parentheses(input), Ok(true));

        // Case 7: Empty string
        let input = "";
        assert_eq!(has_outer_parentheses(input), Ok(false));

        // Case 8: String without parentheses
        let input = "A and B and C";
        assert_eq!(has_outer_parentheses(input), Ok(false));

        // Case 9: String with only one parenthesis
        let input = "(";
        assert_eq!(
            has_outer_parentheses(input),
            Err(ParseError::UnbalancedParentheses)
        );

        // Case 10: String with only one parenthesis
        let input = ")";
        assert_eq!(
            has_outer_parentheses(input),
            Ok(false)
        );

        // Case 11: String with multiple nested parentheses
        let input = "((A and B) and (C or D))";
        assert_eq!(has_outer_parentheses(input), Ok(true));

        // Case 12: String with mismatched parentheses
        let input = "((A and B) and (C or D)";
        assert_eq!(
            has_outer_parentheses(input),
            Err(ParseError::UnbalancedParentheses)
        );
    }

    #[test]
    fn test_get_package_format() {
        let test_url = "https://mirrors.huaweicloud.com/ubuntu//pool/main/u/ubuntu-themes/ubuntu-mono_24.04-0ubuntu1_all.deb";
        match get_package_format(test_url) {
            Some(format) => println!("包格式: {}", format),
            None => println!("无法确定包格式"),
        }
        assert_eq!(get_package_format(test_url), Some("deb".to_string()));
        let rpm_url = "https://repo.openeuler.org/openEuler-24.09/everything/aarch64/Packages/http_load-09Mar2016-1.oe2409.aarch64.rpm";
        assert_eq!(get_package_format(rpm_url), Some("rpm".to_string()));
    }
}
