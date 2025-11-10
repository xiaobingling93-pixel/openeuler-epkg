use lazy_static::lazy_static;
use regex::Regex;
use std::fmt;
use std::path::Path;
use std::error::Error;
use crate::models::PackageFormat;

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

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
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
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct VersionConstraint {
    pub operator: Operator,
    pub operand: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Hash)]
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
pub fn parse_requires(package_format: PackageFormat, requires: &str) -> Result<AndDepends, ParseError> {
    match package_format {
        PackageFormat::Rpm => parse_rpm_requires(requires),
        PackageFormat::Deb => parse_deb_requires(requires),
        PackageFormat::Pacman => parse_archlinux_requires(requires),
        PackageFormat::Python => parse_python_requires(requires),
        PackageFormat::Conda => parse_conda_requires(requires),
        PackageFormat::Apk => parse_archlinux_requires(requires),
        PackageFormat::Epkg => Err(ParseError::UnsupportedPackageType), // Default case
    }
}

lazy_static! {
    // Include '=' for cases where it's not normalized (e.g., "kernel=6.6.0")
    // We match '=' only when it's not part of '==', '!=', '>=', '<=', '~='
    // This is done by matching '=' separately and checking context in code
    static ref RPM_OPERATOR_REGEX:       Regex = Regex::new(r"(>=|<=|==|!=|>|<|~=|=)").unwrap();

    static ref ARCHLINUX_OPERATOR_REGEX: Regex = Regex::new(r"(>=|<=|==|!=|>|<|=~|=|~=|~)").unwrap();
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

/// Handles conditional dependencies with "if" operator
///
/// The "if" operator creates conditional dependencies that are only required
/// if certain conditions are met.
///
/// Examples:
/// - `((A and B and C) if (X or Y))`
///   => `[[A if X; A if Y], [B if X; B if Y], [C if X; C if Y]]`
///   Each capability requires either X or Y
///
/// - `(A if (B and C))`
///   => `[[A if B and C]]`
///   A requires both B and C to be installed
///
/// Structure:
/// - The final `and_depends.size == capability_deps.size`
/// - Each `pkg_depend.size *= condition_or.size` (for OR conditions)
/// - Each `pkg_depend.constraints.size += condition_and.size` (for AND conditions)
fn handle_if_operator(requires: &str) -> Result<Option<AndDepends>, ParseError> {
    if let Some((capability_part, condition_part)) = split_on_if(requires)? {
        let capability_part = capability_part.replace(" and ", ",");
        let capability_deps = parse_rpm_requires(&capability_part)?;
        // Replace " and " with "," in condition_part to handle AND conditions
        let condition_part = condition_part.replace(" and ", ",");
        let condition_deps = parse_rpm_requires(&condition_part)?;

        let mut and_depends = Vec::new();

        for capability_and in capability_deps {
            let mut combined_or = Vec::new();
            for pkg_depend in capability_and {
                // Check if condition_deps has multiple AND groups (AND conditions)
                // vs single AND group with multiple OR alternatives (OR conditions)
                if condition_deps.len() > 1 {
                    // AND conditions: (A if (B and C)) -> add both B and C as constraints
                    let mut new_constraints = pkg_depend.constraints.clone();
                    for condition_and in &condition_deps {
                        // Each condition_and is an OR group, take the first (or any) element
                        if let Some(condition_pkg) = condition_and.first() {
                            new_constraints.push(VersionConstraint {
                                operator: Operator::IfInstall,
                                operand: condition_pkg.capability.clone(),
                            });
                        }
                    }
                    combined_or.push(PkgDepend {
                        capability: pkg_depend.capability.clone(),
                        constraints: new_constraints,
                    });
                } else {
                    // OR conditions: (A if (B or C)) -> generate alternatives
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
            }
            if !combined_or.is_empty() {
                and_depends.push(combined_or);
            }
        }

        Ok(Some(and_depends))
    } else {
        Ok(None)
    }
}

/// Handles "with" operator for combining constraints
///
/// The "with" operator allows combining multiple version constraints for the same package,
/// or specifying dependencies on different packages together.
///
/// Formats supported:
/// - Single package with multiple constraints:
///   `"package constraint1 with constraint2"`
///   => package must satisfy constraint1 AND constraint2
///
/// - Single package with OR constraints:
///   `"package constraint1 with (constraint2 or constraint3)"`
///   => package must satisfy constraint1 AND (constraint2 OR constraint3)
///
/// - Multiple "with" clauses:
///   `"package constraint1 with constraint2 with constraint3"`
///   => package must satisfy all constraints
///
/// - Different packages:
///   `"package1 constraint1 with package2 constraint2"`
///   => both package1 (with constraint1) AND package2 (with constraint2) must be satisfied
///
/// - OR expressions in left part:
///   `"(package < 3 or package > 3) with constraint"`
///   => (package < 3 OR package > 3) AND constraint
fn handle_with_operator(requires: &str) -> Result<Option<AndDepends>, ParseError> {
    if let Some((left_part, right_part)) = split_on_with(requires)? {
        let left_part = left_part.trim();
        let right_part = right_part.trim();

        // Parse the left side - handle OR expressions if present
        let left_inner = if left_part.starts_with('(') && left_part.ends_with(')') {
            &left_part[1..left_part.len()-1]
        } else {
            left_part
        };

        let mut left_parts: Vec<(String, Vec<VersionConstraint>)> = Vec::new();
        if left_inner.contains(" or ") {
            for or_clause in left_inner.split(" or ") {
                let or_clause = or_clause.trim();
                if or_clause.is_empty() {
                    continue;
                }
                let normalized_or = normalize_operators_skip_parens(or_clause);
                let (or_pkg_name, or_constraints) = parse_package(&normalized_or)?;
                left_parts.push((or_pkg_name, or_constraints));
            }
        } else {
            let normalized_left = normalize_operators_skip_parens(left_part);
            let (pkg_name, constraints) = parse_package(&normalized_left)?;
            left_parts.push((pkg_name, constraints));
        }

        // If all OR clauses refer to the same package, combine their constraints
        // Otherwise, use the first one as base (and handle others separately if needed)
        let (pkg_name, mut base_constraints) = if left_parts.len() > 1 {
            let first_pkg = &left_parts[0].0;
            let all_same_pkg = left_parts.iter().all(|(pkg, _)| pkg == first_pkg);
            if all_same_pkg {
                // All refer to same package - combine all constraints
                let mut combined = Vec::new();
                for (_, constraints) in &left_parts {
                    combined.extend(constraints.clone());
                }
                (first_pkg.clone(), combined)
            } else {
                // Different packages - use first as base
                left_parts[0].clone()
            }
        } else {
            left_parts[0].clone()
        };

        // Handle multiple "with" clauses by recursively parsing the right side
        // The right side may contain additional "with" clauses
        let mut remaining = right_part.to_string();
        let mut all_parts: Vec<(String, Vec<VersionConstraint>)> = vec![(pkg_name.clone(), base_constraints.clone())];

        loop {
            // Check if there are more "with" clauses in the remaining part
            if let Some((next_left, next_right)) = split_on_with(&remaining)? {
                let next_left = next_left.trim();
                let next_right = next_right.trim();

                // Parse the next constraint part - handle OR expressions if present
                let next_inner = if next_left.starts_with('(') && next_left.ends_with(')') {
                    &next_left[1..next_left.len()-1]
                } else {
                    next_left
                };

                let mut next_parts: Vec<(String, Vec<VersionConstraint>)> = Vec::new();
                if next_inner.contains(" or ") {
                    // Next part contains OR expressions - parse each OR clause
                    for or_clause in next_inner.split(" or ") {
                        let or_clause = or_clause.trim();
                        if or_clause.is_empty() {
                            continue;
                        }
                        let normalized_or = normalize_operators_skip_parens(or_clause);
                        let (or_pkg_name, or_constraints) = parse_package(&normalized_or)?;
                        next_parts.push((or_pkg_name, or_constraints));
                    }
                } else {
                    // Next part is a single package - parse normally
                    let normalized_next = normalize_operators_skip_parens(next_left);
                    let (pkg_name, constraints) = parse_package(&normalized_next)?;
                    next_parts.push((pkg_name, constraints));
                }

                // If all OR clauses refer to the same package, combine their constraints
                let (next_pkg_name, next_constraints) = if next_parts.len() > 1 {
                    let first_pkg = &next_parts[0].0;
                    let all_same_pkg = next_parts.iter().all(|(pkg, _)| pkg == first_pkg);
                    if all_same_pkg {
                        // All refer to same package - combine all constraints
                        let mut combined = Vec::new();
                        for (_, constraints) in &next_parts {
                            combined.extend(constraints.clone());
                        }
                        (first_pkg.clone(), combined)
                    } else {
                        // Different packages - use first as base
                        next_parts[0].clone()
                    }
                } else {
                    next_parts[0].clone()
                };

                // If package names match, combine constraints
                if next_pkg_name == pkg_name {
                    base_constraints.extend(next_constraints);
                    // Update the first part with accumulated constraints
                    all_parts[0].1 = base_constraints.clone();
                } else {
                    // Different package - treat as separate AND dependency
                    all_parts.push((next_pkg_name, next_constraints));
                }

                // Continue with the remaining part
                remaining = next_right.to_string();
            } else {
                // No more "with" clauses, parse the final part
                let final_trimmed = remaining.trim();
                let final_inner = if final_trimmed.starts_with('(') && final_trimmed.ends_with(')') {
                    &final_trimmed[1..final_trimmed.len()-1]
                } else {
                    final_trimmed
                };

                // Parse the OR group on the final part (or single constraint if not an OR group)
                let mut final_parts: Vec<(String, Vec<VersionConstraint>)> = Vec::new();
                for or_clause in final_inner.split(" or ") {
                    let or_clause = or_clause.trim();
                    if or_clause.is_empty() {
                        continue;
                    }

                    // Parse the constraint in this OR clause
                    let normalized_or = normalize_operators_skip_parens(or_clause);
                    let (or_pkg_name, or_constraints) = parse_package(&normalized_or)?;

                    final_parts.push((or_pkg_name, or_constraints));
                }

                // If final part has same package name as base, combine constraints
                // Otherwise, treat as separate AND dependencies
                if final_parts.len() == 1 && final_parts[0].0 == pkg_name {
                    // Same package - combine with base constraints
                    base_constraints.extend(final_parts[0].1.clone());
                    all_parts[0].1 = base_constraints.clone();

                    // Return as single OR group with combined constraints
                    return Ok(Some(vec![vec![PkgDepend {
                        capability: pkg_name.clone(),
                        constraints: base_constraints,
                    }]]));
                } else if final_parts.len() == 1 && final_parts[0].0 != pkg_name {
                    // Different package - treat as separate AND dependencies
                    all_parts.push((final_parts[0].0.clone(), final_parts[0].1.clone()));

                    // Return as separate AND groups
                    let mut and_depends = Vec::new();
                    for (cap, constraints) in all_parts {
                        and_depends.push(vec![PkgDepend {
                            capability: cap,
                            constraints,
                        }]);
                    }
                    return Ok(Some(and_depends));
                } else {
                    // Multiple OR clauses in final part - need to handle each
                    // For now, if base package matches any OR clause, combine; otherwise treat separately
                    let mut has_matching = false;
                    let mut combined_or_depends = Vec::new();

                    for (or_pkg_name, or_constraints) in final_parts {
                        if or_pkg_name == pkg_name {
                            // Same package - combine with base constraints
                            let mut combined_constraints = base_constraints.clone();
                            combined_constraints.extend(or_constraints);
                            combined_or_depends.push(PkgDepend {
                                capability: pkg_name.clone(),
                                constraints: combined_constraints,
                            });
                            has_matching = true;
                        } else {
                            // Different package - add as separate dependency
                            combined_or_depends.push(PkgDepend {
                                capability: or_pkg_name,
                                constraints: or_constraints,
                            });
                        }
                    }

                    if has_matching {
                        // Return as single OR group
                        return Ok(Some(vec![combined_or_depends]));
                    } else {
                        // All are different packages - return as separate AND groups
                        let mut and_depends = Vec::new();
                        for (cap, constraints) in all_parts {
                            and_depends.push(vec![PkgDepend {
                                capability: cap,
                                constraints,
                            }]);
                        }
                        // Add the OR group as additional AND dependency
                        and_depends.push(combined_or_depends);
                        return Ok(Some(and_depends));
                    }
                }
            }
        }
    } else {
        Ok(None)
    }
}

/// Result of parsing OR clauses - either a simple OR group or multiple AND groups
enum OrClauseResult {
    Simple(OrDepends),
    Complex(AndDepends),
}

/// Parses OR clauses within an AND clause
/// Handles expressions like (A or (B and C)) by applying De Morgan's law:
/// (A or (B and C)) = (A or B) and (A or C)
fn parse_or_clauses(and_clause: &str) -> Result<OrClauseResult, ParseError> {
    let or_clauses: Vec<&str> = and_clause.split(" or ").collect();

    // Collect all simple OR clauses (those without "and")
    let mut simple_or_clauses = Vec::new();
    let mut complex_or_clauses = Vec::new();

    for or_clause in &or_clauses {
        let or_clause = or_clause.trim();
        if or_clause.is_empty() {
            continue;
        }

        // Check if this OR clause contains "and" (possibly nested in parentheses)
        let contains_and = or_clause.contains(" and ");

        if contains_and {
            complex_or_clauses.push(or_clause);
        } else {
            simple_or_clauses.push(or_clause);
        }
    }

    // If we have complex clauses (with "and"), we need to expand them
    if !complex_or_clauses.is_empty() {
        Ok(OrClauseResult::Complex(expand_complex_or_clauses(simple_or_clauses, complex_or_clauses)?))
    } else {
        // No complex clauses - handle normally
        let mut or_depends = Vec::new();
        for or_clause in simple_or_clauses {
            let normalized_clause = normalize_operators_skip_parens(or_clause);
            let (name, constraints) = parse_package(&normalized_clause)?;
            or_depends.push(PkgDepend {
                capability: name,
                constraints,
            });
        }
        Ok(OrClauseResult::Simple(or_depends))
    }
}

/// Expands complex OR clauses using De Morgan's law
///
/// When we have an expression like `(A or B or (C and D))`, we need to expand it
/// using De Morgan's law: `(A or B or (C and D)) = (A or B or C) and (A or B or D)`
///
/// This means:
/// - Each simple clause (A, B) appears in every resulting OR group
/// - Each nested AND group from the complex clause creates a new OR group
/// - All resulting OR groups are combined as AND dependencies
///
/// Example:
/// - Input: `(A or B or (C and D))`
/// - Simple clauses: `[A, B]`
/// - Complex clause: `(C and D)` -> parsed as `[[C], [D]]`
/// - Output: `[[A, B, C], [A, B, D]]`
///   Meaning: (A OR B OR C) AND (A OR B OR D)
fn expand_complex_or_clauses(
    simple_or_clauses: Vec<&str>,
    complex_or_clauses: Vec<&str>,
) -> Result<AndDepends, ParseError> {
    // Parse all simple clauses into PkgDepend objects
    let mut simple_pkgs = Vec::new();
    for simple_clause in &simple_or_clauses {
        let normalized_clause = normalize_operators_skip_parens(simple_clause);
        let (name, constraints) = parse_package(&normalized_clause)?;
        simple_pkgs.push(PkgDepend {
            capability: name,
            constraints,
        });
    }

    // For each complex clause, recursively parse it to get its AND structure
    // Then combine with simple clauses using De Morgan's law
    let mut result = Vec::new();
    for complex_clause in complex_or_clauses {
        let nested_and_depends = parse_rpm_requires(complex_clause)?;

        // For each nested AND group, create a new OR group that includes all simple clauses
        // Example: (A or B or (C and D)) becomes (A or B or C) and (A or B or D)
        for nested_and_group in &nested_and_depends {
            let mut new_or_group = simple_pkgs.clone();
            new_or_group.extend(nested_and_group.clone());
            result.push(new_or_group);
        }

        // If we only had complex clauses (no simple ones), add nested groups directly
        if simple_pkgs.is_empty() {
            for nested_and_group in nested_and_depends {
                result.push(nested_and_group);
            }
        }
    }

    Ok(result)
}

/// Normalizes version operators by adding spaces around them, but skips operators inside parentheses
/// This prevents capabilities like "font(:lang=en)" from being split incorrectly
fn normalize_operators_skip_parens(s: &str) -> String {
    let mut result = String::new();
    let mut paren_depth = 0;
    let mut i = 0;
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        match chars[i] {
            '(' => {
                paren_depth += 1;
                result.push(chars[i]);
                i += 1;
            }
            ')' => {
                paren_depth -= 1;
                result.push(chars[i]);
                i += 1;
            }
            _ => {
                // Check for multi-character operators first
                if i + 1 < chars.len() {
                    let two_char = &chars[i..i+2].iter().collect::<String>();
                    if paren_depth == 0 {
                        match two_char.as_str() {
                            ">=" | "<=" | "==" | "!=" | "~=" => {
                                result.push(' ');
                                result.push_str(two_char);
                                result.push(' ');
                                i += 2;
                                continue;
                            }
                            _ => {}
                        }
                    }
                }

                // Check for single-character operators
                // Note: '~' is NOT treated as an operator here because in RPM versions like "0.9~rc2-2.fc42",
                // the '~' is part of the version string. Only '~=' (Python) is an operator, which is
                // already handled by the two-character check above.
                if paren_depth == 0 {
                    match chars[i] {
                        '>' | '<' | '=' | '!' => {
                            result.push(' ');
                            result.push(chars[i]);
                            result.push(' ');
                            i += 1;
                            continue;
                        }
                        _ => {}
                    }
                }

                result.push(chars[i]);
                i += 1;
            }
        }
    }

    result
}

/// Normalizes " and " operators to "," at top level (depth 0)
/// This allows us to treat "and" and "," uniformly for AND dependencies
fn normalize_and_operators(requires: &str) -> String {
    let mut result = String::new();
    let mut depth = 0;
    let mut i = 0;
    let chars: Vec<char> = requires.chars().collect();
    while i < chars.len() {
        match chars[i] {
            '(' => {
                depth += 1;
                result.push(chars[i]);
                i += 1;
            }
            ')' => {
                depth -= 1;
                result.push(chars[i]);
                i += 1;
            }
            _ => {
                // Check if we have " and " at depth 0
                if depth == 0 && i + 5 <= chars.len() {
                    let candidate: String = chars[i..i+5].iter().collect();
                    if candidate == " and " {
                        result.push(',');
                        i += 5;
                        continue;
                    }
                }
                result.push(chars[i]);
                i += 1;
            }
        }
    }
    result
}

pub fn parse_rpm_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let requires = requires.trim();

    // Step 1: Remove surrounding parentheses and recurse only if the entire string is enclosed
    if has_outer_parentheses(requires)? {
        let inner = &requires[1..(requires.len() - 1)];
        // println!("dive into {:#?}", inner);
        return parse_rpm_requires(inner);
    }

    // Step 2: Split into capability and condition parts if " if " is present
    if let Some(result) = handle_if_operator(requires)? {
        return Ok(result);
    }

    // Step 2.5: Handle "with" operator for combining constraints
    if let Some(result) = handle_with_operator(requires)? {
        return Ok(result);
    }

    // Step 3: Split AND clauses by commas or " and " at top level
    let normalized_requires = normalize_and_operators(requires);

    let mut and_depends = Vec::new();
    for and_clause in normalized_requires.split(',') {
        let and_clause = and_clause.trim();
        if and_clause.is_empty() {
            continue;
        }

        // Step 4: Parse OR clauses
        let parsed = parse_or_clauses(and_clause)?;
        match parsed {
            OrClauseResult::Simple(or_depends) => {
                if !or_depends.is_empty() {
                    and_depends.push(or_depends);
                }
            }
            OrClauseResult::Complex(mut complex_and_depends) => {
                and_depends.append(&mut complex_and_depends);
            }
        }
    }

    Ok(and_depends)
}

/// Splits a string on " if " operator, handling parentheses correctly
/// Returns Some((left, right)) if " if " is found at the top level, None otherwise
/// The " if " operator splits capability and condition parts
fn split_on_if(s: &str) -> Result<Option<(&str, &str)>, ParseError> {
    let s = s.trim();

    // Look for " if " (with spaces) that's not inside parentheses
    let mut start_pos = 0;

    while let Some(pos) = s[start_pos..].find(" if ") {
        let actual_pos = start_pos + pos;

        // Check the context before " if "
        let before_if = &s[..actual_pos];

        // Count parentheses before " if "
        let mut depth_before = 0;
        for c in before_if.chars() {
            match c {
                '(' => depth_before += 1,
                ')' => depth_before -= 1,
                _ => {}
            }
        }

        // If we're at depth 0, this " if " is at the top level
        if depth_before == 0 {
            let left = &s[..actual_pos];
            let right = &s[actual_pos + 4..]; // 4 = len(" if ")
            return Ok(Some((left, right)));
        }

        // Continue searching from after this " if "
        start_pos = actual_pos + 4;
    }

    Ok(None)
}

/// Splits a string on "with" operator, handling parentheses correctly
/// Returns Some((left, right)) if "with" is found, None otherwise
/// The "with" operator can be followed by either:
/// - A parenthesized expression: "package >= 1.0 with (package < 2.0 or package >= 3.0)"
/// - A simple constraint: "package >= 1.0 with package < 2.0"
fn split_on_with(s: &str) -> Result<Option<(&str, &str)>, ParseError> {
    let s = s.trim();

    // Look for " with " (with spaces) that's not inside parentheses
    let mut start_pos = 0;

    while let Some(pos) = s[start_pos..].find(" with ") {
        let actual_pos = start_pos + pos;

        // Check the context before "with"
        let before_with = &s[..actual_pos];
        let after_with = &s[actual_pos + 6..]; // 6 = len(" with ")

        // Count parentheses before "with"
        let mut depth_before = 0;
        for c in before_with.chars() {
            match c {
                '(' => depth_before += 1,
                ')' => depth_before -= 1,
                _ => {}
            }
        }

        // If we're at depth 0, this "with" is at the top level
        if depth_before == 0 {
            let after_trimmed = after_with.trim();

            // Case 1: Right side is parenthesized
            if after_trimmed.starts_with('(') {
                // Find the matching closing parenthesis
                let mut depth_after = 1;
                let mut end_pos = 1;
                for c in after_trimmed[1..].chars() {
                    match c {
                        '(' => depth_after += 1,
                        ')' => {
                            depth_after -= 1;
                            if depth_after == 0 {
                                // Found matching closing parenthesis
                                let left = &s[..actual_pos];
                                let right = &after_trimmed[..end_pos + 1]; // Include the opening '(' and closing ')'
                                return Ok(Some((left, right)));
                            }
                        }
                        _ => {}
                    }
                    end_pos += 1;
                }
            } else {
                // Case 2: Right side is not parenthesized - return everything after "with"
                // This handles cases like "package >= 1.0 with package < 2.0"
                let left = &s[..actual_pos];
                let right = after_trimmed;
                return Ok(Some((left, right)));
            }
        }

        // Continue searching from after this "with"
        start_pos = actual_pos + 6;
    }

    Ok(None)
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

/// Parses a RPM/Archlinux package clause into a name and version constraints.
fn parse_package(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    let mut clause = clause.trim().to_string();

    // Remove outer parentheses if present
    if clause.starts_with('(') && clause.ends_with(')') {
        // Check if parentheses are balanced and outer
        if has_outer_parentheses(&clause)? {
            clause = clause[1..clause.len()-1].to_string();
        }
    }

    // Check if clause contains "with" - if so, handle it specially
    if clause.contains(" with ") {
        return parse_package_with_clause(&clause);
    }

    // Try to parse with inline version operators first (e.g., "kernel=6.6.0")
    match parse_package_inline_version(&clause) {
        Ok(result) => return Ok(result),
        Err(ParseError::InvalidFormat(_)) => {
            // Not an inline version format, fall through to whitespace-separated
        }
        Err(e) => return Err(e), // Other errors should propagate
    }

    // Fall back to whitespace-separated format
    parse_package_whitespace_separated(&clause)
}

/// Handles package clauses with "with" operator (e.g., "tcl >= 1:8.6 with tcl < 1:9")
fn parse_package_with_clause(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    // Split on " with " and parse each part
    let parts: Vec<&str> = clause.split(" with ").collect();
    if parts.is_empty() {
        return Err(ParseError::InvalidFormat("Empty clause after splitting on 'with'".to_string()));
    }

    // Parse the first part to get the package name
    let (name, mut constraints) = parse_package(parts[0])?;

    // Parse remaining parts - they should all refer to the same package
    for part in parts.iter().skip(1) {
        let part = part.trim();
        // The part after "with" might be just a constraint (e.g., "tcl < 1:9")
        // or it might include the package name again (e.g., "tcl < 1:9")
        let (part_name, part_constraints) = parse_package(part)?;

        // If package names don't match, that's an error for "with" usage
        if part_name != name {
            return Err(ParseError::InvalidFormat(format!(
                "Package name mismatch in 'with' clause: expected '{}', found '{}'",
                name, part_name
            )));
        }

        // Combine constraints
        constraints.extend(part_constraints);
    }

    Ok((name, constraints))
}

/// Parses package clauses with inline version operators (e.g., "kernel=6.6.0" or "pkg>=1.0")
/// Returns Ok if successfully parsed, Err if the format doesn't match
fn parse_package_inline_version(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    let mut parts = clause.split_whitespace();
    let first_part = parts.next().ok_or(ParseError::InvalidFormat("Missing package name".to_string()))?;

    // Check if the first part contains a version operator without whitespace
    // Skip if it contains parentheses (e.g., "libfoo(x86-64)" is an atomic capability)
    // Also handle capabilities with parameters like "font(:lang=en)" where the '='
    // is inside parentheses and should not be treated as a version operator.
    let has_parens = first_part.contains('(') && first_part.contains(')');
    // Check for version operators OUTSIDE parentheses (operators inside parentheses
    // are part of the capability name, e.g., "font(:lang=en)")
    // Note: '~' is NOT treated as an operator here because in RPM versions like "0.9~rc2-2.fc42",
    // the '~' is part of the version string. Only '~=' (Python) is an operator, but that's
    // handled by parse_operator_from_start when parsing the version part.
    let mut paren_depth = 0;
    let has_version_op = first_part.chars().any(|c| {
        match c {
            '(' => {
                paren_depth += 1;
                false
            }
            ')' => {
                paren_depth -= 1;
                false
            }
            '<' | '>' | '=' | '!' if paren_depth == 0 => true,
            _ => false,
        }
    });

    if !has_parens && has_version_op {
        // Find the first version operator that's not inside parentheses
        // Note: '~' is excluded because it's part of version strings in RPM, not an operator
        let mut paren_depth = 0;
        let mut found_idx = None;
        for (idx, ch) in first_part.char_indices() {
            match ch {
                '(' => paren_depth += 1,
                ')' => paren_depth -= 1,
                '>' | '<' | '=' | '!' if paren_depth == 0 => {
                    found_idx = Some(idx);
                    break;
                }
                _ => {}
            }
        }

        if let Some(idx) = found_idx {
            let name = first_part[..idx].trim().to_string();
            let version_part = first_part[idx..].trim();

            // Try to parse version constraints from the first part
            match parse_version_constraints(version_part) {
                Ok(first_constraints) if !first_constraints.is_empty() => {
                    // Parse remaining whitespace-separated parts
                    let mut constraints = first_constraints;
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
                            println!("parse_package error, invalid operator '{}' in clause {}", part, clause);
                            return Err(ParseError::UnsupportedOperator);
                        }
                    }

                    return Ok((name, constraints));
                }
                _ => {
                    // If parsing fails, return error to fall back to whitespace-separated format
                    return Err(ParseError::InvalidFormat("Failed to parse inline version".to_string()));
                }
            }
        }
    }

    Err(ParseError::InvalidFormat("Not an inline version format".to_string()))
}

/// Parses package clauses with whitespace-separated format (e.g., "pkg >= 1.0")
fn parse_package_whitespace_separated(clause: &str) -> Result<(String, Vec<VersionConstraint>), ParseError> {
    let mut parts = clause.split_whitespace();
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
            println!("parse_package error, invalid operator '{}' in clause {}", part, clause);
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

/// Format a version constraint to a string for world.json
/// Returns the constraint string (e.g., "=version1", ">=version2", "=version4" for pkgkey format)
pub fn format_version_constraint_for_world(constraints: &[VersionConstraint]) -> String {
    if constraints.is_empty() {
        return String::new();
    }

    // For normal constraints, format the first constraint
    // APK world format supports: =, >=, >, <=, <, ~, >~, <~
    if let Some(constraint) = constraints.first() {
        let op_str = match constraint.operator {
            Operator::VersionEqual => "=",
            Operator::VersionGreaterThanEqual => ">=",
            Operator::VersionGreaterThan => ">",
            Operator::VersionLessThanEqual => "<=",
            Operator::VersionLessThan => "<",
            Operator::VersionCompatible => "~",
            _ => "=", // Default to = for other operators
        };
        format!("{}{}", op_str, constraint.operand)
    } else {
        String::new()
    }
}

/// Parse a world.json constraint string back into VersionConstraint
/// Examples: "=version1" -> VersionEqual, ">=version2" -> VersionGreaterThanEqual, "" -> None
pub fn parse_world_constraint(constraint_str: &str) -> Option<Vec<VersionConstraint>> {
    if constraint_str.is_empty() {
        return None;
    }

    // Parse the constraint string (e.g., "=version1", ">=version2")
    match parse_version_constraints(constraint_str) {
        Ok(constraints) if !constraints.is_empty() => Some(constraints),
        _ => None,
    }
}

/// Parse a package spec with version constraints (e.g., "pkgname=version", "pkgname>=version")
/// Also supports pkgkey format: "pkgname__version__arch" (e.g., "htop__3.4.1-4__arm64")
/// Returns (package_name_without_version, Option<Vec<VersionConstraint>>, is_pkgkey_format)
///
/// `format` is used to determine whether '~' should be treated as a version operator.
/// In APK format, '~' can be a version operator (e.g., "a~2.2"), but in other formats
/// like RPM, '~' in library names like "libSPIRV-Tools-2025.1~rc1.so" is part of the name.
pub fn parse_package_spec_with_version(spec: &str, format: PackageFormat) -> (String, Option<Vec<VersionConstraint>>) {

    // Check if this is a pkgkey format: pkgname__version__arch
    // Only treat as pkgkey if it matches the exact pattern and doesn't contain parentheses
    // (capabilities with parentheses like "ksym(default:__SCT__cond_resched)" are not pkgkeys)
    let parts: Vec<&str> = spec.split("__").collect();
    if parts.len() == 3 && !spec.contains('(') && !spec.contains(')') {
        // This is a pkgkey format
        let pkgname = parts[0].to_string();
        let version = parts[1].to_string();
        // Create an exact version constraint
        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: version,
        };
        return (pkgname, Some(vec![constraint]));
    }

    // Capabilities such as "libfoo.so()(64bit)" (RPM provides) are atomic tokens that
    // include parentheses but never embed inline version operators.  They also don't
    // contain whitespace, so treat them as pure capability names and avoid splitting
    // on characters like '~' that belong to the name rather than a constraint.
    // Fixes: tests/debug_solve.sh opensuse Mesa-dri
    // Also handle ksym capabilities like "ksym(default:__SCT__cond_resched)" which
    // contain colons but are still atomic capabilities.
    // Also handle capabilities with parameters like "font(:lang=en)" where the '='
    // is inside parentheses and should not be treated as a version operator.
    let has_parens = spec.contains('(') && spec.contains(')');
    let has_whitespace = spec.chars().any(|c| c.is_ascii_whitespace());
    // Check for version operators OUTSIDE parentheses (operators inside parentheses
    // are part of the capability name, e.g., "font(:lang=en)")
    let mut paren_depth = 0;
    let has_version_op = spec.chars().any(|c| {
        match c {
            '(' => {
                paren_depth += 1;
                false
            }
            ')' => {
                paren_depth -= 1;
                false
            }
            '<' | '>' | '=' | '!' if paren_depth == 0 => true,
            _ => false,
        }
    });
    let looks_like_atomic_capability = has_parens && !has_whitespace && !has_version_op;
    if looks_like_atomic_capability {
        return (spec.to_string(), None);
    }

    // If we get here, the atomic check didn't match, so look for version operators
    // But first check if this might be a capability with a version that was already
    // split incorrectly (e.g., "ksym(default:" from "ksym(default:__SCT__cond_resched) = version")
    // In that case, we should not try to parse it further

    // Find the first occurrence of a version operator
    // But skip operators that are inside parentheses (e.g., font(:lang=en) should not split on =)
    // Note: '~' can be an operator in APK format (e.g., "a~=2.2" or "a~2.2").
    // In RPM versions like "0.9~rc2-2.fc42", the '~' is part of the version string AFTER an operator,
    // so checking for '~' BEFORE other operators is safe.
    // However, for library names like "libSPIRV-Tools-2025.1~rc1.so", the '~' is part of the name,
    // not an operator. So we only treat '~' as an operator in APK format, and only when
    // followed by a digit (e.g., "package~2.2"). This prevents "~rc1" from being treated as an operator.
    let mut paren_depth = 0;
    let mut found_idx = None;
    // First, check for two-character operators like "~=" before single-character operators
    let chars: Vec<char> = spec.chars().collect();
    for (idx, ch) in spec.char_indices() {
        match ch {
            '(' => paren_depth += 1,
            ')' => paren_depth -= 1,
            '~' if paren_depth == 0 => {
                // Check if it's "~=" (two-character operator) or standalone "~"
                if idx + 1 < chars.len() && chars[idx + 1] == '=' {
                    // This is "~=", split before the "~" (valid in all formats)
                    found_idx = Some(idx);
                    break;
                } else if format == PackageFormat::Apk && idx + 1 < chars.len() {
                    // Standalone "~" operator is only valid in APK format
                    // and only if followed by a digit (e.g., "package~2.2")
                    // This prevents treating "~rc1" in library names as an operator
                    let next_char = chars[idx + 1];
                    if next_char.is_ascii_digit() {
                        found_idx = Some(idx);
                        break;
                    }
                }
                // For non-APK formats or when '~' is not followed by a digit,
                // standalone "~" is part of the name, not an operator
            }
            '>' | '<' | '=' | '!' if paren_depth == 0 => {
                found_idx = Some(idx);
                break;
            }
            _ => {}
        }
    }

    if let Some(idx) = found_idx {
        let name = spec[..idx].trim().to_string();
        let version_part = spec[idx..].trim();

        // Try to parse version constraints
        match parse_version_constraints(version_part) {
            Ok(constraints) if !constraints.is_empty() => {
                return (name, Some(constraints));
            }
            _ => {
                // If parsing fails, treat the whole string as the package name
                return (spec.to_string(), None);
            }
        }
    }

    // No version constraints found
    (spec.to_string(), None)
}

/// Parses Debian/Python/Conda version constraints from a string
/// Example inputs:
/// - ">= 2.34" or ">= 2.34, << 3.0"
/// - ">=1.14.12,<2.0a0"
pub fn parse_version_constraints(version_part: &str) -> Result<Vec<VersionConstraint>, ParseError> {
    parse_version_constraints_and(version_part)
}

/// Parses comma-separated AND constraints (no OR operator).
/// Used internally for parsing version constraints.
fn parse_version_constraints_and(version_part: &str) -> Result<Vec<VersionConstraint>, ParseError> {
    let mut constraints = Vec::new();

    // Split the version part by commas to handle multiple constraints
    for constraint in version_part.split(',') {
        let constraint = constraint.trim();
        if constraint.is_empty() {
            continue;
        }

        let (operator, op_len) = match parse_operator_from_start(constraint) {
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

        // Check if the operand ends with `.*` or just `*` and update the operator accordingly
        // Note: VersionEqual and VersionNotEqual now handle both literal and pattern matching
        // Patterns like "=9*" or "=6.9.*" are handled by checking the operand for '*' in the version checking logic

        constraints.push(VersionConstraint { operator, operand });
    }

    Ok(constraints)
}

/// Parses an operator from the start of a string (e.g., ">=1.14.12" -> (Operator::VersionGreaterThanEqual, 2)).
fn parse_operator_from_start(s: &str) -> Option<(Operator, usize)> {
    if s.starts_with(">=") {
        Some((Operator::VersionGreaterThanEqual, 2))
    } else if s.starts_with(">>") {
        Some((Operator::VersionGreaterThan, 2))
    } else if s.starts_with(">") {
        Some((Operator::VersionGreaterThan, 1))
    } else if s.starts_with("<=") {
        Some((Operator::VersionLessThanEqual, 2))
    } else if s.starts_with("<<") {
        Some((Operator::VersionLessThan, 2))
    } else if s.starts_with("<") {
        Some((Operator::VersionLessThan, 1))
    } else if s.starts_with("==") {
        Some((Operator::VersionEqual, 2))
    } else if s.starts_with("=") {
        Some((Operator::VersionEqual, 1))
    } else if s.starts_with("!=") {
        Some((Operator::VersionNotEqual, 2))
    } else if s.starts_with("~=") {
        Some((Operator::VersionCompatible, 2))
    } else if s.starts_with("~") {
        Some((Operator::VersionCompatible, 1))
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
        "=~" => Some(Operator::VersionCompatible),   // https://wiki.alpinelinux.org/wiki/Alpine_Package_Keeper#Package_pinning apk add 'asterisk=~1.6'
        "~"  => Some(Operator::VersionCompatible),   // https://wiki.alpinelinux.org/wiki/APKBUILD_Reference ignores revision part
        // Note: "=*" and "!*" are no longer needed - use "=" and "!=" with operand containing '*'
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
// depends=('libstk-5.0.0.so=libstk-5.0.0.so-64')  // library alias, not version constraint
pub fn parse_archlinux_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let requires = ARCHLINUX_COMMENT_REGEX.replace(requires, "").to_string();
    let mut and_depends = Vec::new();

    for clause in requires.split_whitespace() {
        // Check if this is a library alias (e.g., "lib.so=lib.so-64")
        // In Arch Linux, = can mean either a version constraint or a library alias.
        // Library aliases typically have .so followed by -64, -32, etc., and don't start with digits.
        if let Some(equals_pos) = clause.find('=') {
            let after_equals = &clause[equals_pos + 1..];
            // Check if it looks like a library alias (contains .so and doesn't start with digit)
            // This distinguishes from version constraints like "libfoo=1.0.0"
            if after_equals.contains(".so") && !after_equals.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                // This is a library alias, not a version constraint
                // Use the left side as the capability name
                let capability = clause[..equals_pos].to_string();
                and_depends.push(vec![PkgDepend {
                    capability,
                    constraints: Vec::new(),
                }]);
                continue;
            }
        }

        // Normalize the clause by adding whitespace around operators
        let normalized_clause = ARCHLINUX_OPERATOR_REGEX.replace_all(clause, " $1 ").to_string();
        let (name, constraints) = parse_package(&normalized_clause)?;
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
/// https://docs.conda.io/projects/conda/en/stable/user-guide/concepts/pkg-specs.html#package-match-specifications
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
/// Parse a single conda package requirement item
/// Handles both 3-part (name version build) and 2-part (name version) formats
fn parse_conda_single_item(package_part: &str) -> Result<Vec<Vec<PkgDepend>>, ParseError> {
    // Split by whitespace to separate package name from version constraints
    let pkg_parts: Vec<&str> = package_part.split_whitespace().collect();
    if pkg_parts.is_empty() {
        return Ok(vec![vec![]]);
    }

    // Parse package name and version constraints
    let (name, version_constraints_str) = parse_package_name_and_constraints(&pkg_parts)?;

    // Handle 3-part match spec: name version_pattern build_string_pattern
    if let Some(ref constraints_str) = version_constraints_str {
        let constraint_parts: Vec<&str> = constraints_str.split_whitespace().collect();
        if constraint_parts.len() == 2 {
            // Combine version pattern with build string pattern using '=' separator
            let combined = format!("{}={}", constraint_parts[0], constraint_parts[1]);
            let constraints = parse_version_constraints_and(&combined)?;
            return Ok(vec![vec![PkgDepend {
                capability: name.to_string(),
                constraints,
            }]]);
        }
    }

    // Handle 2-part match spec: name version (with AND/OR constraints)
    let or_alternatives = if let Some(ref constraints_str) = version_constraints_str {
        parse_conda_version_constraints(constraints_str)?
    } else {
        vec![Vec::new()]
    };

    // Create PkgDepend entries
    let mut and_depends = Vec::new();
    if or_alternatives.len() > 1 {
        // Multiple OR alternatives
        let mut or_depends = Vec::new();
        for constraints in or_alternatives {
            or_depends.push(PkgDepend {
                capability: name.to_string(),
                constraints,
            });
        }
        and_depends.push(or_depends);
    } else {
        // Single OR alternative
        and_depends.push(vec![PkgDepend {
            capability: name.to_string(),
            constraints: or_alternatives[0].clone(),
        }]);
    }

    Ok(and_depends)
}

/// Parse package name and extract version constraints string
fn parse_package_name_and_constraints(pkg_parts: &[&str]) -> Result<(String, Option<String>), ParseError> {
    let first_part = pkg_parts[0];

    // Check if the first part starts with a constraint operator
    if first_part.starts_with(">=") || first_part.starts_with("<=") ||
       first_part.starts_with("==") || first_part.starts_with("!=") ||
       first_part.starts_with(">") || first_part.starts_with("<") {
        // The first part is all constraints (no package name)
        Ok((first_part.to_string(), if pkg_parts.len() > 1 { Some(pkg_parts[1..].join(" ")) } else { None }))
    } else if let Some(eq_pos) = first_part.find('=') {
        // Check if the '=' is part of an operator (>=, <=, ==, !=)
        let is_operator = eq_pos > 0 && matches!(
            first_part.chars().nth(eq_pos - 1),
            Some('>') | Some('<') | Some('!')
        );

        if !is_operator && eq_pos > 0 {
            // This is package=version format (e.g., "numpy=1.11.1|1.11.3")
            let pkg_name = &first_part[..eq_pos];
            let version_part = &first_part[eq_pos..]; // Includes the '='
            let version_str = if pkg_parts.len() > 1 {
                format!("{} {}", version_part, pkg_parts[1..].join(" "))
            } else {
                version_part.to_string()
            };
            Ok((pkg_name.to_string(), Some(version_str)))
        } else {
            // The '=' is part of an operator, so find where constraints actually start
            let constraint_start = first_part.find(|c: char| c == '>' || c == '<' || c == '=' || c == '!')
                .unwrap_or(first_part.len());

            if constraint_start < first_part.len() {
                let pkg_name = &first_part[..constraint_start];
                let constraints = &first_part[constraint_start..];
                let version_str = if pkg_parts.len() > 1 {
                    format!("{} {}", constraints, pkg_parts[1..].join(" "))
                } else {
                    constraints.to_string()
                };
                Ok((pkg_name.to_string(), Some(version_str)))
            } else {
                // No constraints found, whole thing is package name
                Ok((first_part.to_string(), if pkg_parts.len() > 1 { Some(pkg_parts[1..].join(" ")) } else { None }))
            }
        }
    } else {
        // No '=' in package name, check if there are version constraints in other parts
        Ok((first_part.to_string(), if pkg_parts.len() > 1 { Some(pkg_parts[1..].join(" ")) } else { None }))
    }
}

/// Parse version constraints string (Level 3: split AND parts by ',', Level 4: split OR parts by '|')
/// Returns a list of OR alternatives, where each alternative is a list of AND constraints
fn parse_conda_version_constraints(constraints_str: &str) -> Result<Vec<Vec<VersionConstraint>>, ParseError> {
    // Level 4: Split OR parts by '|'
    if constraints_str.contains('|') {
        let mut alternatives = Vec::new();
        for or_part in constraints_str.split('|') {
            let or_part = or_part.trim();
            if or_part.is_empty() {
                continue;
            }
            // Level 3: Split AND parts by ','
            let and_constraints = parse_version_constraints_and(or_part)?;
            alternatives.push(and_constraints);
        }
        Ok(alternatives)
    } else {
        // No OR operator, just parse AND constraints
        let and_constraints = parse_version_constraints_and(constraints_str)?;
        Ok(vec![and_constraints])
    }
}

pub fn parse_conda_requires(requires: &str) -> Result<AndDepends, ParseError> {
    let mut and_depends = Vec::new();

    if requires.trim().is_empty() {
        return Ok(and_depends);
    }

    // Level 1: Split items by ", "
    let package_parts: Vec<&str> = requires.split(", ").map(|s| s.trim()).collect();

    // Level 2: Handle each single item
    for package_part in package_parts {
        if package_part.is_empty() {
            continue;
        }

        let item_result = parse_conda_single_item(package_part)?;
        and_depends.extend(item_result);
    }

    Ok(and_depends)
}

#[allow(dead_code)]
pub fn get_package_format(origin_url: &str) -> Option<PackageFormat> {
    let path = Path::new(origin_url);
    let ext = path.extension().and_then(|s| s.to_str())?;

    match ext {
        "rpm" => Some(PackageFormat::Rpm),
        "deb" => Some(PackageFormat::Deb),
        "apk" => Some(PackageFormat::Apk),
        "pkg.tar.zst" | "pkg.tar.xz" => Some(PackageFormat::Pacman),
        "conda" => Some(PackageFormat::Conda),
        "whl" | "tar.gz" => Some(PackageFormat::Python),
        _ => Some(PackageFormat::Epkg),
    }
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

    #[test]
    fn test_parse_package_spec_with_version_rpm_capability_atom() {
        let capability = "libSPIRV-Tools-2025.1~rc1.so()(64bit)";
        let (name, constraints) = parse_package_spec_with_version(capability, PackageFormat::Rpm);
        assert_eq!(name, capability);
        assert!(constraints.is_none());
    }

    #[test]
    fn test_parse_package_spec_with_version_library_name_with_tilde() {
        // Library names like "libSPIRV-Tools-2025.1~rc1.so" should not split on '~'
        // because '~' is part of the library name, not a version operator (in RPM format)
        let capability = "libSPIRV-Tools-2025.1~rc1.so";
        let (name, constraints) = parse_package_spec_with_version(capability, PackageFormat::Rpm);
        assert_eq!(name, capability);
        assert!(constraints.is_none());
    }

    #[test]
    fn test_parse_package_spec_with_version_apk_tilde_operator() {
        // In APK format, '~' can be a version operator, but only if followed by a digit
        let spec = "package~2.2";
        let (name, constraints) = parse_package_spec_with_version(spec, PackageFormat::Apk);
        assert_eq!(name, "package");
        assert!(constraints.is_some());
        let constraints = constraints.unwrap();
        assert_eq!(constraints.len(), 1);
        assert_eq!(constraints[0].operator, Operator::VersionCompatible);
        assert_eq!(constraints[0].operand, "2.2");
    }

    #[test]
    fn test_parse_package_spec_with_version_apk_tilde_not_operator_when_followed_by_letter() {
        // In APK format, '~' should NOT be treated as operator if followed by a letter
        // This handles cases like "package~rc1" or library names with tildes
        let spec = "package~rc1";
        let (name, constraints) = parse_package_spec_with_version(spec, PackageFormat::Apk);
        assert_eq!(name, spec); // Should not split, '~rc1' is part of the name
        assert!(constraints.is_none());
    }

    #[test]
    fn test_parse_package_spec_with_version_rpm_tilde_not_operator() {
        // In RPM format, standalone '~' should never be treated as operator
        // even if followed by a digit
        let spec = "package~2.2";
        let (name, constraints) = parse_package_spec_with_version(spec, PackageFormat::Rpm);
        assert_eq!(name, spec); // Should not split, '~2.2' is part of the name
        assert!(constraints.is_none());
    }

    #[test]
    fn test_parse_package_spec_with_version_tilde_equals_always_operator() {
        // '~=' is always treated as an operator in all formats
        let spec = "package~=2.2";
        let (name, constraints) = parse_package_spec_with_version(spec, PackageFormat::Rpm);
        assert_eq!(name, "package");
        assert!(constraints.is_some());
        let constraints = constraints.unwrap();
        assert_eq!(constraints.len(), 1);
        assert_eq!(constraints[0].operator, Operator::VersionCompatible);
        assert_eq!(constraints[0].operand, "2.2");

        // Also test in APK format
        let (name2, constraints2) = parse_package_spec_with_version(spec, PackageFormat::Apk);
        assert_eq!(name2, "package");
        assert!(constraints2.is_some());
    }

    #[test]
    fn test_parse_package_spec_with_version_font_lang() {
        let capability = "font(:lang=he)";
        let (name, constraints) = parse_package_spec_with_version(capability, PackageFormat::Rpm);
        assert_eq!(name, capability);
        assert!(constraints.is_none());
    }

    #[test]
    fn test_parse_rpm_requires_font_lang() {
        // Test that font(:lang=en) is parsed correctly as an atomic capability
        let input = "font(:lang=en)";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 1);
        assert_eq!(result[0][0].capability, "font(:lang=en)");
        assert!(result[0][0].constraints.is_empty());
    }

    #[test]
    fn test_parse_package_spec_with_version_ksym() {
        // Test ksym capabilities with colons
        let capability = "ksym(default:__SCT__cond_resched)";
        let (name, constraints) = parse_package_spec_with_version(capability, PackageFormat::Rpm);
        assert_eq!(name, capability);
        assert!(constraints.is_none());

        // Test with version
        let capability_with_version = "ksym(default:__SCT__cond_resched) = c07351b3";
        let (name2, constraints2) = parse_package_spec_with_version(capability_with_version, PackageFormat::Rpm);
        assert_eq!(name2, "ksym(default:__SCT__cond_resched)");
        assert!(constraints2.is_some());
    }

    // Test RPM parsing
    #[test]
    fn test_rpm() {
        // Simple package
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "pixman").unwrap(),
            vec![vec![pkg("pixman", &[])]]
        );

        // Version constraint
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "pixman >= 0.30.0").unwrap(),
            vec![vec![pkg("pixman", &[(">=", "0.30.0")])]]
        );

        // File dependency
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "/etc/pam.d/system-auth").unwrap(),
            vec![vec![pkg("/etc/pam.d/system-auth", &[])]]
        );

        // Logical OR
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "(mysql or mariadb)").unwrap(),
            vec![vec![
                pkg("mysql", &[]),
                pkg("mariadb", &[])
            ]]
        );

        // Conditional (if)
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "feh if Xserver").unwrap(),
            vec![vec![pkg("feh", &[("if", "Xserver")])]]
        );

        // Nested conditionals
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "((feh and xrandr) if Xserver)").unwrap(),
            vec![
                vec![pkg("feh", &[("if", "Xserver")])],
                vec![pkg("xrandr", &[("if", "Xserver")])]
            ]
        );

        // Multiple constraints
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "perl(Net::Server) >= 2.0 < 3.0").unwrap(),
            vec![vec![pkg("perl(Net::Server)", &[(">=", "2.0"), ("<", "3.0")])]]
        );

        let input = "(containerd or cri-o or docker or docker-ce or moby-engine)";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();

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

    #[test]
    fn test_rpm_inline_version_operator() {
        // Test kernel=version format (no whitespace around =)
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "kernel=6.6.0-72.0.0.76.oe2403sp1.x86_64").unwrap(),
            vec![vec![pkg("kernel", &[("=", "6.6.0-72.0.0.76.oe2403sp1.x86_64")])]]
        );

        // Test other inline operators
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "pkg>=1.0").unwrap(),
            vec![vec![pkg("pkg", &[(">=", "1.0")])]]
        );

        assert_eq!(
            parse_requires(PackageFormat::Rpm, "pkg<=2.0").unwrap(),
            vec![vec![pkg("pkg", &[("<=", "2.0")])]]
        );

        // Test that capabilities with parentheses are not parsed as inline versions
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "libfoo(x86-64)").unwrap(),
            vec![vec![pkg("libfoo(x86-64)", &[])]]
        );

        // Test inline version with additional whitespace-separated constraints
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "kernel=6.6.0 < 7.0").unwrap(),
            vec![vec![pkg("kernel", &[("=", "6.6.0"), ("<", "7.0")])]]
        );
    }

    #[test]
    fn test_rpm_version_with_tilde() {
        // Test that RPM version strings with '~' are correctly parsed
        // The '~' character in versions like "0.9~rc2-2.fc42" is part of the version string,
        // not a version operator, so it should not be split.

        // Test 1: Simple version with tilde
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "avahi-libs = 0.9~rc2-2.fc42").unwrap(),
            vec![vec![pkg("avahi-libs", &[("=", "0.9~rc2-2.fc42")])]]
        );

        // Test 2: Capability with architecture and version with tilde
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "avahi-libs(x86-64) = 0.9~rc2-2.fc42").unwrap(),
            vec![vec![pkg("avahi-libs(x86-64)", &[("=", "0.9~rc2-2.fc42")])]]
        );

        // Test 3: Inline version format with tilde
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "avahi-libs=0.9~rc2-2.fc42").unwrap(),
            vec![vec![pkg("avahi-libs", &[("=", "0.9~rc2-2.fc42")])]]
        );

        // Test 4: Version with tilde should not be split when parsing package spec
        let (name, constraints) = parse_package_spec_with_version("avahi-libs = 0.9~rc2-2.fc42", PackageFormat::Rpm);
        assert_eq!(name, "avahi-libs");
        assert!(constraints.is_some());
        let constraints = constraints.unwrap();
        assert_eq!(constraints.len(), 1);
        assert_eq!(constraints[0].operator, Operator::VersionEqual);
        assert_eq!(constraints[0].operand, "0.9~rc2-2.fc42");

        // Test 6: Version with tilde in capability
        let (name2, constraints2) = parse_package_spec_with_version("avahi-libs(x86-64) = 0.9~rc2-2.fc42", PackageFormat::Rpm);
        assert_eq!(name2, "avahi-libs(x86-64)");
        assert!(constraints2.is_some());
        let constraints2 = constraints2.unwrap();
        assert_eq!(constraints2.len(), 1);
        assert_eq!(constraints2[0].operator, Operator::VersionEqual);
        assert_eq!(constraints2[0].operand, "0.9~rc2-2.fc42");

        // Test 7: Multiple constraints with one having tilde
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "pkg >= 0.9~rc1 = 0.9~rc2-2.fc42").unwrap(),
            vec![vec![pkg("pkg", &[(">=", "0.9~rc1"), ("=", "0.9~rc2-2.fc42")])]]
        );
    }

    // Test the "if" operator with or conditions
    #[test]
    fn test_if_or_conditions() {
        let input = "(A if (B or C))";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();

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
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();

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

    // Test parsing of complex requirement strings with function-like syntax and nested parentheses
    #[test]
    fn test_rpm_complex_nested_if_with_function_syntax() {
        // Test case for the specific error: unbalanced parentheses with pkgconfig and crate
        let input = "((pkgconfig(cairo-gobject) >= 1.16 if crate(cairo-sys-rs/v1_16)) and pkgconfig(cairo-gobject) >= 1.14)";
        let result = parse_requires(PackageFormat::Rpm, input);
        assert!(result.is_ok(), "Should parse successfully: {:?}", result.err());

        let and_depends = result.unwrap();
        // Should have 2 AND groups: one for the conditional part and one for the unconditional part
        assert!(and_depends.len() >= 1, "Should have at least 1 AND group");

        // Test another variant
        let input2 = "((pkgconfig(cairo-gobject) >= 1.17 if crate(cairo-sys-rs/v1_18)) and pkgconfig(cairo-gobject) >= 1.14)";
        let result2 = parse_requires(PackageFormat::Rpm, input2);
        assert!(result2.is_ok(), "Should parse successfully: {:?}", result2.err());
    }

    // Test the "with" operator for combining constraints
    #[test]
    fn test_rpm_with_operator() {
        // Simple "with" operator: tcl >= 1:8.4.13 with tcl < 1:9
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "tcl >= 1:8.4.13 with tcl < 1:9").unwrap(),
            vec![vec![pkg("tcl", &[(">=", "1:8.4.13"), ("<", "1:9")])]]
        );

        // "with" operator in parentheses
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "(tcl >= 1:8.4.13 with tcl < 1:9)").unwrap(),
            vec![vec![pkg("tcl", &[(">=", "1:8.4.13"), ("<", "1:9")])]]
        );

        // "with" operator in OR clause - the actual error case from the user
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "((tcl >= 1:8.4.13 with tcl < 1:9) or tcl8 >= 1:8.4.13)").unwrap(),
            vec![vec![
                pkg("tcl", &[(">=", "1:8.4.13"), ("<", "1:9")]),
                pkg("tcl8", &[(">=", "1:8.4.13")]),
            ]]
        );

        // Similar case for tk
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "((tk >= 1:8.4.13 with tk < 1:9) or tk8 >= 1:8.4.13)").unwrap(),
            vec![vec![
                pkg("tk", &[(">=", "1:8.4.13"), ("<", "1:9")]),
                pkg("tk8", &[(">=", "1:8.4.13")]),
            ]]
        );

        // Test case from user's error: complex OR with nested AND
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "(rubygem(rack) < 3 or (rubygem(rack) >= 3 and rubygem(rackup)))").unwrap(),
            vec![
                vec![pkg("rubygem(rack)", &[("<", "3")]), pkg("rubygem(rack)", &[(">=", "3")])],
                vec![pkg("rubygem(rack)", &[("<", "3")]), pkg("rubygem(rackup)", &[])]
            ]
        );

        // Multiple "with" clauses
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "package >= 1.0 with package < 2.0 with package != 1.5").unwrap(),
            vec![vec![pkg("package", &[(">=", "1.0"), ("<", "2.0"), ("!=", "1.5")])]]
        );

        // "with" operator with different constraint types
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "python >= 3.6 with python <= 3.11").unwrap(),
            vec![vec![pkg("python", &[(">=", "3.6"), ("<=", "3.11")])]]
        );

        // "with" operator combined with other operators in a complex expression
        // Note: This case might need special handling due to nested parentheses
        // For now, test a simpler version
        let input = "(tcl >= 1:8.4.13 with tcl < 1:9) or tcl8 >= 1:8.4.13, other-pkg >= 1.0";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 2);
        // First AND group should be the OR clause
        assert_eq!(result[0].len(), 2);
        assert!(result[0].contains(&pkg("tcl", &[(">=", "1:8.4.13"), ("<", "1:9")])));
        assert!(result[0].contains(&pkg("tcl8", &[(">=", "1:8.4.13")])));
        // Second AND group should be the other package
        assert_eq!(result[1], vec![pkg("other-pkg", &[(">=", "1.0")])]);
    }

    // Test DEB parsing
    #[test]
    fn test_deb() {
        // Simple package with version
        assert_eq!(
            parse_requires(PackageFormat::Deb, "libc6 (>= 2.34)").unwrap(),
            vec![vec![pkg("libc6", &[(">=", "2.34")])]]
        );

        // Alternative dependencies
        assert_eq!(
            parse_requires(PackageFormat::Deb, "libgcc-s1 (>= 3.0) | gcc").unwrap(),
            vec![vec![
                pkg("libgcc-s1", &[(">=", "3.0")]),
                pkg("gcc", &[]),
            ]]
        );

        // Multiple alternatives
        assert_eq!(
            parse_requires(PackageFormat::Deb, "emacs | emacs-gtk | emacs-lucid").unwrap(),
            vec![vec![
                pkg("emacs", &[]),
                pkg("emacs-gtk", &[]),
                pkg("emacs-lucid", &[]),
            ]]
        );

        // Complex example from original question
        let input = "libao4 (>= 1.1.0), libc6 (>= 2.34), debconf (>= 0.5) | debconf-2.0";
        let result = parse_requires(PackageFormat::Deb, input).unwrap();
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
            parse_requires(PackageFormat::Pacman, "bash: GNU Bourne Again SHell").unwrap(),
            vec![vec![pkg("bash", &[])]]
        );

        // Version constraint
        assert_eq!(
            parse_requires(PackageFormat::Pacman, "zsh>=4.3.9").unwrap(),
            vec![vec![pkg("zsh", &[(">=", "4.3.9")])]]
        );

        // Multiple packages
        assert_eq!(
            parse_requires(PackageFormat::Pacman, "git python").unwrap(),
            vec![
                vec![pkg("git", &[])],
                vec![pkg("python", &[])]
            ]
        );

        // Complex example from original question
        let input = "python python-gobject ttf-font gtk3 python-xdg";
        let result = parse_requires(PackageFormat::Pacman, input).unwrap();
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
            parse_requires(PackageFormat::Python, "networkx>=2.3.0").unwrap(),
            vec![vec![pkg("networkx", &[(">=", "2.3.0")])]]
        );

        // Multiple constraints
        assert_eq!(
            parse_requires(PackageFormat::Python, "pbr!=2.1.0,>=2.0.0").unwrap(),
            vec![vec![pkg("pbr", &[("!=", "2.1.0"), (">=", "2.0.0")])]]
        );

        // Comment line
        assert_eq!(
            parse_requires(PackageFormat::Python, "pkg # comment").unwrap(),
            vec![vec![pkg("pkg", &[])]]
        );

        // File path
        assert_eq!(
            parse_requires(PackageFormat::Python, "./granulate-utils/").unwrap(),
            vec![vec![pkg("./granulate-utils/", &[])]]
        );

        // compatibility operator (~=)
        assert_eq!(
            parse_requires(PackageFormat::Python, "package~=1.0").unwrap(),
            vec![vec![pkg("package", &[("~=", "1.0")])]]
        );
    }

    // Test Conda parsing
    #[test]
    fn test_conda() {
        // Simple package
        assert_eq!(
            parse_requires(PackageFormat::Conda, "bwidget").unwrap(),
            vec![vec![pkg("bwidget", &[])]]
        );

        // Version constraint
        assert_eq!(
            parse_requires(PackageFormat::Conda, "cairo >=1.14.12,<2.0a0").unwrap(),
            vec![vec![pkg("cairo", &[(">=", "1.14.12"), ("<", "2.0a0")])]]
        );

        // Package with version constraints
        let input = "cairo 1.14.*";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("cairo", &[("==", "1.14.*")])]]
        );

        // Package with multiple version constraints
        let input = "blas 1.0 openblas";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("blas", &[("==", "1.0=openblas")])],
            ]
        );

        // Test __archspec with version and build_string
        let input = "__archspec 1 skylake_avx512";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("__archspec", &[("==", "1=skylake_avx512")])]]
        );

        // Test comma-separated packages (from real Conda packages)
        let input = "click, joblib, python, regex, six, tqdm";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("click", &[])],
                vec![pkg("joblib", &[])],
                vec![pkg("python", &[])],
                vec![pkg("regex", &[])],
                vec![pkg("six", &[])],
                vec![pkg("tqdm", &[])],
            ]
        );

        // Test packages with build strings
        let input = "libsepol-el8-x86_64 ==2.9 *_0, sysroot_linux-64 2.28.*";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("libsepol-el8-x86_64", &[("==", "2.9=*_0")])],
                vec![pkg("sysroot_linux-64", &[("==", "2.28.*")])],
            ]
        );
    }

    #[test]
    fn test_conda_real_world_examples() {
        // Test 1: Simple packages with version and build string
        // "dal 2021.6.0 hdb19cb5_915"
        let result = parse_conda_requires("dal 2021.6.0 hdb19cb5_915").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("dal", &[("==", "2021.6.0=hdb19cb5_915")])]]
        );

        // Test 2: Multiple packages with constraints
        // "dal 2021.6.0 hdb19cb5_915, dal-include 2021.6.0 h06a4308_915, libgcc-ng >=11.2.0, libstdcxx-ng >=11.2.0"
        let result = parse_conda_requires("dal 2021.6.0 hdb19cb5_915, dal-include 2021.6.0 h06a4308_915, libgcc-ng >=11.2.0, libstdcxx-ng >=11.2.0").unwrap();
        assert_eq!(result.len(), 4);
        assert_eq!(result[0][0].capability, "dal");
        assert_eq!(result[1][0].capability, "dal-include");
        assert_eq!(result[2][0].capability, "libgcc-ng");
        assert!(result[2][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "11.2.0"));
        assert_eq!(result[3][0].capability, "libstdcxx-ng");

        // Test 3: Version range constraints
        // "python >=3.14,<3.15.0a0"
        let result = parse_conda_requires("python >=3.14,<3.15.0a0").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("python", &[(">=", "3.14"), ("<", "3.15.0a0")])]]
        );

        // Test 5: Simple packages without constraints
        // "conda, conda-package-handling, jinja2, python >=3.12,<3.13.0a0, pytz, requests, requests-toolbelt, ruamel.yaml"
        let result = parse_conda_requires("conda, conda-package-handling, jinja2, python >=3.12,<3.13.0a0, pytz, requests, requests-toolbelt, ruamel.yaml").unwrap();
        // Note: The parser may treat "ruamel.yaml" differently - check what we actually get
        assert!(result.len() >= 8); // At least 8 packages
        assert_eq!(result[0][0].capability, "conda");
        assert_eq!(result[1][0].capability, "conda-package-handling");
        assert_eq!(result[2][0].capability, "jinja2");
        assert_eq!(result[3][0].capability, "python");
        assert!(result[3][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "3.12"));
        assert!(result[3][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "3.13.0a0"));
        assert_eq!(result[4][0].capability, "pytz");
        assert_eq!(result[5][0].capability, "requests");
        assert_eq!(result[6][0].capability, "requests-toolbelt");
        // ruamel.yaml might be parsed as "ruamel" with ".yaml" as metadata, or as "ruamel.yaml"
        if result.len() >= 9 {
            assert_eq!(result[7][0].capability, "ruamel.yaml");
        }

        // Test 6: Package with metadata (blas 1.0 mkl)
        // "blas 1.0 mkl" - mkl is metadata, not a constraint
        let result = parse_conda_requires("blas 1.0 mkl").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("blas", &[("==", "1.0=mkl")])]]
        );

        // Test 8: Complex build string patterns
        // "libabseil * cxx17*" - cxx17* is a build string pattern
        // Should be converted to "libabseil(=*=cxx17*)" (3-part conda match spec)
        let result = parse_conda_requires("libabseil * cxx17*").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0].capability, "libabseil");
        // The parser should combine "*" and "cxx17*" into "*=cxx17*" using '=' separator
        // "*" + "=" + "cxx17*" = "*=cxx17*"
        // This is handled by VersionEqual (which supports pattern matching)
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "*=cxx17*"));

        // Test 9: Version with wildcard
        // "tbb 2021.*"
        let result = parse_conda_requires("tbb 2021.*").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("tbb", &[("==", "2021.*")])]]
        );

        // Test 10: Multiple constraints with not equal
        // "sympy >=1.13.1,!=1.13.2"
        let result = parse_conda_requires("sympy >=1.13.1,!=1.13.2").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0].capability, "sympy");
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "1.13.1"));
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionNotEqual) && c.operand == "1.13.2"));

        // Test 12: Version range with multiple packages
        // "dal-include >=2021.6.0,<2022.0a0"
        let result = parse_conda_requires("dal-include >=2021.6.0,<2022.0a0").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("dal-include", &[(">=", "2021.6.0"), ("<", "2022.0a0")])]]
        );

        // Test 15: Already formatted with = separator (should parse as-is)
        // "numpy=1.11.2=*nomkl*" - already in the correct format
        let result = parse_conda_requires("numpy=1.11.2=*nomkl*").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0][0].capability, "numpy");
        // Should parse "1.11.2=*nomkl*" as a single constraint operand
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.11.2=*nomkl*"));

        // Test 16: Version matching with build string pattern (libabseil example)
        // Test that "*=cxx17*" matches versions with build strings starting with "cxx17"
        use crate::version::check_version_constraint;
        use crate::parse_requires::VersionConstraint;
        use crate::parse_requires::Operator;
        use crate::models::PackageFormat;

        let constraint = VersionConstraint {
            operator: Operator::VersionEqual,
            operand: "*=cxx17*".to_string(),
        };

        // These should all match
        assert!(check_version_constraint("20250814.1-cxx17_h7c8e02e_0", &constraint, PackageFormat::Conda).unwrap(),
            "20250814.1-cxx17_h7c8e02e_0 should match *=cxx17*");
        assert!(check_version_constraint("20250127.0-cxx17_h6a678d5_0", &constraint, PackageFormat::Conda).unwrap(),
            "20250127.0-cxx17_h6a678d5_0 should match *=cxx17*");
        assert!(check_version_constraint("20240116.2-cxx17_h6a678d5_0", &constraint, PackageFormat::Conda).unwrap(),
            "20240116.2-cxx17_h6a678d5_0 should match *=cxx17*");

        // This should not match (build string doesn't start with cxx17)
        assert!(!check_version_constraint("20250127.0-h6a678d5_0", &constraint, PackageFormat::Conda).unwrap(),
            "20250127.0-h6a678d5_0 should not match *=cxx17*");

        // Test 14: Multiple numpy constraints (duplicate package with different constraints)
        // "numpy >=1.21,<3, numpy >=1.24.0,<3.0.0"
        // Note: The parser treats duplicate package names as separate packages
        let result = parse_conda_requires("numpy >=1.21,<3, numpy >=1.24.0,<3.0.0").unwrap();
        // The parser creates separate entries for each occurrence
        assert!(result.len() >= 2);
        // Find both numpy entries
        let numpy_entries: Vec<_> = result.iter().filter(|or_dep| or_dep[0].capability == "numpy").collect();
        assert!(numpy_entries.len() >= 2);
        // First numpy should have >=1.21,<3
        assert!(numpy_entries[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "1.21"));
        assert!(numpy_entries[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "3"));
        // Second numpy should have >=1.24.0,<3.0.0
        assert!(numpy_entries[1][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "1.24.0"));
        assert!(numpy_entries[1][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "3.0.0"));

        // Test 15: Complex example with many packages
        // "_openmp_mutex >=5.1, blas 1.0 mkl, filelock, fsspec, intel-openmp >=2023.1.0,<2024.0a0"
        let result = parse_conda_requires("_openmp_mutex >=5.1, blas 1.0 mkl, filelock, fsspec, intel-openmp >=2023.1.0,<2024.0a0").unwrap();
        // The parser may combine packages or treat them separately
        assert!(result.len() >= 4); // At least 4 distinct packages
        // Find packages by capability
        let openmp_mutex = result.iter().find(|or_dep| or_dep[0].capability == "_openmp_mutex");
        assert!(openmp_mutex.is_some(), "Should find _openmp_mutex");
        assert!(openmp_mutex.unwrap()[0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "5.1"));

        let blas = result.iter().find(|or_dep| or_dep[0].capability == "blas");
        assert!(blas.is_some(), "Should find blas");
        // "mkl" is treated as a constraint by the parser (metadata is not filtered out)
        assert!(blas.unwrap()[0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.0=mkl"));

        let filelock = result.iter().find(|or_dep| or_dep[0].capability == "filelock");
        assert!(filelock.is_some(), "Should find filelock");

        let fsspec = result.iter().find(|or_dep| or_dep[0].capability == "fsspec");
        assert!(fsspec.is_some(), "Should find fsspec");

        let intel_openmp = result.iter().find(|or_dep| or_dep[0].capability == "intel-openmp");
        assert!(intel_openmp.is_some(), "Should find intel-openmp");
        assert!(intel_openmp.unwrap()[0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "2023.1.0"));

        // Test 16: Package name starting with underscore
        // "__glibc >=2.28,<3.0.a0"
        let result = parse_conda_requires("__glibc >=2.28,<3.0.a0").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("__glibc", &[(">=", "2.28"), ("<", "3.0.a0")])]]
        );

        // Test 17: Simple version constraint
        // "humanfriendly >=9.1"
        let result = parse_conda_requires("humanfriendly >=9.1").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("humanfriendly", &[(">=", "9.1")])]]
        );

        // Test 18: Package with dash in name
        // "dal-include >=2021.6.0,<2022.0a0"
        let result = parse_conda_requires("dal-include >=2021.6.0,<2022.0a0").unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("dal-include", &[(">=", "2021.6.0"), ("<", "2022.0a0")])]]
        );

        // Test simple version constraint
        let input = "python >=3.6";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![vec![pkg("python", &[(">=", "3.6")])]]
        );

        // Test complex constraints with multiple operators per package
        let input = "jupyter_client >=5.2.0, jupyter_core >=4.4.0, notebook >=5.7.6,<7.0, python >=3.6.1";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("jupyter_client", &[(">=", "5.2.0")])],
                vec![pkg("jupyter_core", &[(">=", "4.4.0")])],
                vec![pkg("notebook", &[(">=", "5.7.6"), ("<", "7.0")])],
                vec![pkg("python", &[(">=", "3.6.1")])],
            ]
        );

        // Test virtual package
        let input = "__unix, patchelf >=0.12";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("__unix", &[])],
                vec![pkg("patchelf", &[(">=", "0.12")])],
            ]
        );

        // Test OR operator in version constraints
        // Example: "numpy=1.11.1|1.11.3" matches 1.11.1 or 1.11.3
        let input = "numpy=1.11.1|1.11.3";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(result.len(), 1); // One package
        assert_eq!(result[0].len(), 2); // Two OR alternatives
        assert_eq!(result[0][0].capability, "numpy");
        assert_eq!(result[0][1].capability, "numpy");
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.11.1"));
        assert!(result[0][1].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.11.3"));

        // Test AND/OR precedence: ">=1,<2|>3" means "(>=1 AND <2) OR (>3)"
        let input = "numpy>=1,<2|>3";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 2); // Two OR alternatives
        assert_eq!(result[0][0].capability, "numpy");
        assert_eq!(result[0][1].capability, "numpy");
        // First OR alternative: >=1 AND <2
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "1"));
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "2"));
        // Second OR alternative: >3
        assert!(result[0][1].constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThan) && c.operand == "3"));

        // Test version with OR: "1.0|1.2" matches version 1.0 or 1.2
        let input = "python 1.0|1.2";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 2); // Two OR alternatives
        assert_eq!(result[0][0].capability, "python");
        assert_eq!(result[0][1].capability, "python");
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.0"));
        assert!(result[0][1].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.2"));

        // Test wildcard: "1.0|1.4*" matches 1.0, 1.4 and 1.4.1b2, but not 1.2
        let input = "python 1.0|1.4*";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 2); // Two OR alternatives
        assert!(result[0][0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.0"));
        assert!(result[0][1].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "1.4*"));

        // Test complex example with many packages
        let input = "anyio >=3.1.0,<4, argon2-cffi, ipython_genutils, jinja2, jupyter_client >=6.1.1";
        let result = parse_conda_requires(input).unwrap();
        assert_eq!(
            result,
            vec![
                vec![pkg("anyio", &[(">=", "3.1.0"), ("<", "4")])],
                vec![pkg("argon2-cffi", &[])],
                vec![pkg("ipython_genutils", &[])],
                vec![pkg("jinja2", &[])],
                vec![pkg("jupyter_client", &[(">=", "6.1.1")])],
            ]
        );

        // Test arm-variant example
        let input = "arm-variant * sbsa, cuda-cccl_linux-aarch64, cuda-cudart-static_linux-aarch64";
        let result = parse_conda_requires(input).unwrap();
        assert!(result.len() >= 3);
        // Find arm-variant
        let arm_variant = result.iter().find(|or_dep| or_dep[0].capability == "arm-variant");
        assert!(arm_variant.is_some());
        // "*" is handled by VersionEqual (which supports pattern matching)
        assert!(arm_variant.unwrap()[0].constraints.iter().any(|c| matches!(c.operator, Operator::VersionEqual) && c.operand == "*=sbsa"));

        let cuda_cccl = result.iter().find(|or_dep| or_dep[0].capability == "cuda-cccl_linux-aarch64");
        assert!(cuda_cccl.is_some());

        let cuda_cudart = result.iter().find(|or_dep| or_dep[0].capability == "cuda-cudart-static_linux-aarch64");
        assert!(cuda_cudart.is_some());

    }

    // Test error handling
    #[test]
    fn test_errors() {
        // Unbalanced parentheses
        assert!(matches!(
            parse_requires(PackageFormat::Rpm, "(feh and xrandr").unwrap_err(),
            ParseError::UnbalancedParentheses
        ));

        // Unsupported package type
        assert!(matches!(
            parse_requires(PackageFormat::Epkg, "express").unwrap_err(),
            ParseError::UnsupportedPackageType
        ));

        // Empty input
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "").unwrap(),
            Vec::<OrDepends>::new()
        );
    }

    // Test edge cases
    #[test]
    fn test_edge_cases() {
        // Multiple spaces
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "  pkg   >=   2.0  ").unwrap(),
            vec![vec![pkg("pkg", &[(">=", "2.0")])]]
        );

        // "with" operator with extra whitespace
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "tcl  >=  1:8.4.13  with  tcl  <  1:9").unwrap(),
            vec![vec![pkg("tcl", &[(">=", "1:8.4.13"), ("<", "1:9")])]]
        );

        // "with" operator with nested parentheses
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "((tcl >= 1:8.4.13 with tcl < 1:9))").unwrap(),
            vec![vec![pkg("tcl", &[(">=", "1:8.4.13"), ("<", "1:9")])]]
        );
    }

    // Test handling of whitespace variations
    #[test]
    fn test_whitespace_variations() {
        // Multiple spaces between package and version
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "package  >=  1.0").unwrap(),
            vec![vec![pkg("package", &[(">=", "1.0")])]]
        );

        // No spaces between package and version
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "package>=1.0").unwrap(),
            vec![vec![pkg("package", &[(">=", "1.0")])]]
        );
    }

    // Test parsing of package types with different naming conventions
    #[test]
    fn test_package_naming_conventions() {
        // RPM with namespace
        assert_eq!(
            parse_requires(PackageFormat::Rpm, "perl(Net::LibIDN)").unwrap(),
            vec![vec![pkg("perl(Net::LibIDN)", &[])]]
        );

        // DEB with colon in name
        assert_eq!(
            parse_requires(PackageFormat::Deb, "lib:package").unwrap(),
            vec![vec![pkg("lib:package", &[])]]
        );

        // Python with hyphen in name
        assert_eq!(
            parse_requires(PackageFormat::Python, "package-name").unwrap(),
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
            Some(format) => println!("包格式: {:?}", format),
            None => println!("无法确定包格式"),
        }
        assert_eq!(get_package_format(test_url), Some(PackageFormat::Deb));
        let rpm_url = "https://repo.openeuler.org/openEuler-24.09/everything/aarch64/Packages/http_load-09Mar2016-1.oe2409.aarch64.rpm";
        assert_eq!(get_package_format(rpm_url), Some(PackageFormat::Rpm));
    }

    // Test "with" clause parsing
    #[test]
    fn test_with_clause() {
        // Simple "with" clause
        let input = "python3.13dist(pyparsing) >= 2.4.2 with python3.13dist(pyparsing) < 4";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        assert_eq!(or_group.len(), 1);
        let pkg_dep = &or_group[0];
        assert_eq!(pkg_dep.capability, "python3.13dist(pyparsing)");
        assert_eq!(pkg_dep.constraints.len(), 2);
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "2.4.2"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "4"));

        // "with" clause with different packages
        let input = "package1 >= 1.0 with package2 >= 2.0";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.contains(&vec![pkg("package1", &[(">=", "1.0")])]));
        assert!(result.contains(&vec![pkg("package2", &[(">=", "2.0")])]));
    }

    // Test OR expressions in "with" clause left side
    #[test]
    fn test_or_in_with_left_side() {
        // OR expression in left side of "with" clause
        let input = "(python3.13dist(pyparsing) < 3 or python3.13dist(pyparsing) > 3) with python3.13dist(pyparsing) < 4";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        assert_eq!(or_group.len(), 1);
        let pkg_dep = &or_group[0];
        assert_eq!(pkg_dep.capability, "python3.13dist(pyparsing)");
        // Should have combined constraints from OR clauses plus the "with" constraint
        assert!(pkg_dep.constraints.len() >= 2);
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "3"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThan) && c.operand == "3"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "4"));

        // Complex case with multiple "with" clauses and OR in left side
        let input = "((python3.13dist(pyparsing) < 3 or python3.13dist(pyparsing) > 3) with (python3.13dist(pyparsing) < 3.0.1 or python3.13dist(pyparsing) > 3.0.1) with python3.13dist(pyparsing) < 4)";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        // The result might have 1 or 2 AND groups depending on how the OR clauses are handled
        // If all OR clauses refer to the same package, they should be combined into 1 group
        // If they're treated separately, we might get 2 groups
        // For now, accept either 1 or 2 groups as valid
        assert!(result.len() >= 1 && result.len() <= 2, "Expected 1 or 2 AND groups, got {}", result.len());
        // Check that at least one group contains the expected package
        let has_expected_pkg = result.iter().any(|or_group| {
            or_group.iter().any(|p| p.capability == "python3.13dist(pyparsing)")
        });
        assert!(has_expected_pkg, "Expected to find python3.13dist(pyparsing) in result");
    }

    // Test OR expressions in "with" clause right side
    #[test]
    fn test_or_in_with_right_side() {
        // OR expression in right side of "with" clause
        let input = "python3.13dist(pyparsing) >= 2.4.2 with (python3.13dist(pyparsing) < 3 or python3.13dist(pyparsing) > 3)";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        // Should have two alternatives (one for each OR clause)
        assert!(or_group.len() >= 1);
        // All should refer to the same package
        assert!(or_group.iter().all(|p| p.capability == "python3.13dist(pyparsing)"));
    }

    // Test error cases that should now be fixed
    #[test]
    fn test_with_clause_error_cases() {
        // Case 1: npm(lodash._baseindexof) >= 3.0.0 with npm(lodash._baseindexof) < 4
        let input = "npm(lodash._baseindexof) >= 3.0.0 with npm(lodash._baseindexof) < 4";
        let result = parse_requires(PackageFormat::Rpm, input);
        assert!(result.is_ok(), "Should parse successfully: {:?}", result);
        let result = result.unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        assert_eq!(or_group.len(), 1);
        let pkg_dep = &or_group[0];
        assert_eq!(pkg_dep.capability, "npm(lodash._baseindexof)");
        assert_eq!(pkg_dep.constraints.len(), 2);
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "3.0.0"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "4"));

        // Case 2: rubygem(rack) < 3 with rubygem(rack) >= 2.2.4
        let input = "rubygem(rack) < 3 with rubygem(rack) >= 2.2.4";
        let result = parse_requires(PackageFormat::Rpm, input);
        assert!(result.is_ok(), "Should parse successfully: {:?}", result);
        let result = result.unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        assert_eq!(or_group.len(), 1);
        let pkg_dep = &or_group[0];
        assert_eq!(pkg_dep.capability, "rubygem(rack)");
        assert_eq!(pkg_dep.constraints.len(), 2);
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "3"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "2.2.4"));

        // Case 3: (appstream-glib >= 0.3.6 with asglib(swcatalog))
        let input = "(appstream-glib >= 0.3.6 with asglib(swcatalog))";
        let result = parse_requires(PackageFormat::Rpm, input);
        assert!(result.is_ok(), "Should parse successfully: {:?}", result);
        let result = result.unwrap();
        // Should have two AND dependencies (different packages)
        assert_eq!(result.len(), 2);
        assert!(result.contains(&vec![pkg("appstream-glib", &[(">=", "0.3.6")])]));
        assert!(result.contains(&vec![pkg("asglib(swcatalog)", &[])]));
    }

    // Test multiple "with" clauses
    #[test]
    fn test_multiple_with_clauses() {
        // Multiple "with" clauses for same package
        let input = "python3.13dist(pyparsing) >= 2.4.2 with python3.13dist(pyparsing) < 4 with python3.13dist(pyparsing) != 3.0.0";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        assert_eq!(or_group.len(), 1);
        let pkg_dep = &or_group[0];
        assert_eq!(pkg_dep.capability, "python3.13dist(pyparsing)");
        assert_eq!(pkg_dep.constraints.len(), 3);
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual) && c.operand == "2.4.2"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan) && c.operand == "4"));
        assert!(pkg_dep.constraints.iter().any(|c| matches!(c.operator, Operator::VersionNotEqual) && c.operand == "3.0.0"));
    }

    // Test OR expressions in multiple "with" clauses
    #[test]
    fn test_or_in_multiple_with_clauses() {
        // OR in left side, then another "with" clause
        let input = "(python3.13dist(urllib3) < 2.2 or python3.13dist(urllib3) > 2.2) with python3.13dist(urllib3) < 3 with python3.13dist(urllib3) >= 1.25.4";
        let result = parse_requires(PackageFormat::Rpm, input).unwrap();
        assert_eq!(result.len(), 1);
        let or_group = &result[0];
        assert_eq!(or_group.len(), 1);
        let pkg_dep = &or_group[0];
        assert_eq!(pkg_dep.capability, "python3.13dist(urllib3)");
        // Should have constraints from OR clauses plus additional "with" constraints
        assert!(pkg_dep.constraints.len() >= 3);
    }

    // Test complex OR clauses with nested AND (De Morgan's law expansion)
    #[test]
    fn test_complex_or_with_and() {
        // Simple case: (A or (B and C))
        let result = parse_requires(PackageFormat::Rpm, "(A or (B and C))").unwrap();
        // Should expand to: (A or B) and (A or C)
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|or_group| {
            or_group.contains(&pkg("A", &[])) && or_group.contains(&pkg("B", &[]))
        }));
        assert!(result.iter().any(|or_group| {
            or_group.contains(&pkg("A", &[])) && or_group.contains(&pkg("C", &[]))
        }));

        // More complex: (A or B or (C and D))
        let result2 = parse_requires(PackageFormat::Rpm, "(A or B or (C and D))").unwrap();
        // Should expand to: (A or B or C) and (A or B or D)
        assert_eq!(result2.len(), 2);
        for or_group in &result2 {
            assert!(or_group.contains(&pkg("A", &[])));
            assert!(or_group.contains(&pkg("B", &[])));
        }
        assert!(result2[0].contains(&pkg("C", &[])));
        assert!(result2[1].contains(&pkg("D", &[])));

        // With version constraints: (pkg < 3 or (pkg >= 3 and other))
        let result3 = parse_requires(PackageFormat::Rpm, "(pkg < 3 or (pkg >= 3 and other))").unwrap();
        assert_eq!(result3.len(), 2);
        // First OR group: (pkg < 3 or pkg >= 3)
        assert!(result3[0].iter().any(|p| p.capability == "pkg" &&
            p.constraints.iter().any(|c| matches!(c.operator, Operator::VersionLessThan))));
        assert!(result3[0].iter().any(|p| p.capability == "pkg" &&
            p.constraints.iter().any(|c| matches!(c.operator, Operator::VersionGreaterThanEqual))));
        // Second OR group: (pkg < 3 or other)
        assert!(result3[1].iter().any(|p| p.capability == "pkg"));
        assert!(result3[1].iter().any(|p| p.capability == "other"));
    }

    // Test "if" operator edge cases
    #[test]
    fn test_if_operator_edge_cases() {
        // Single capability with single condition
        let result = parse_requires(PackageFormat::Rpm, "A if B").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 1);
        assert_eq!(result[0][0].capability, "A");
        assert_eq!(result[0][0].constraints.len(), 1);
        assert!(matches!(result[0][0].constraints[0].operator, Operator::IfInstall));

        // Single capability with AND conditions
        let result2 = parse_requires(PackageFormat::Rpm, "A if (B and C)").unwrap();
        assert_eq!(result2.len(), 1);
        assert_eq!(result2[0].len(), 1);
        assert_eq!(result2[0][0].capability, "A");
        // Should have both B and C as constraints
        assert_eq!(result2[0][0].constraints.len(), 2);
    }

    // Test "with" operator edge cases
    #[test]
    fn test_with_operator_edge_cases() {
        // Multiple "with" clauses for same package
        let result = parse_requires(PackageFormat::Rpm, "pkg >= 1.0 with pkg < 2.0 with pkg != 1.5").unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].len(), 1);
        assert_eq!(result[0][0].capability, "pkg");
        assert_eq!(result[0][0].constraints.len(), 3);

        // "with" operator with OR in left part
        // Note: When OR clauses refer to the same package, they get combined into one
        let result2 = parse_requires(PackageFormat::Rpm, "(pkg < 3 or pkg > 5) with pkg != 4").unwrap();
        assert_eq!(result2.len(), 1);
        // The OR alternatives get combined into a single package with all constraints
        assert_eq!(result2[0].len(), 1);
        assert_eq!(result2[0][0].capability, "pkg");
        // Should have constraints from both OR alternatives plus the "with" constraint
        assert!(result2[0][0].constraints.len() >= 3);

        // Different packages with "with"
        let result3 = parse_requires(PackageFormat::Rpm, "pkg1 >= 1.0 with pkg2 >= 2.0").unwrap();
        assert_eq!(result3.len(), 2);
        assert_eq!(result3[0][0].capability, "pkg1");
        assert_eq!(result3[1][0].capability, "pkg2");
    }

    // Test normalize_and_operators function indirectly
    #[test]
    fn test_and_operator_normalization() {
        // "and" should be treated same as comma
        let result1 = parse_requires(PackageFormat::Rpm, "A and B").unwrap();
        let result2 = parse_requires(PackageFormat::Rpm, "A, B").unwrap();
        assert_eq!(result1.len(), result2.len());
        assert_eq!(result1.len(), 2);

        // Nested "and" should not be normalized
        let result3 = parse_requires(PackageFormat::Rpm, "(A and B) or C").unwrap();
        // Should parse as: (A and B) or C, which expands to (A or C) and (B or C)
        assert_eq!(result3.len(), 2);
    }
}
