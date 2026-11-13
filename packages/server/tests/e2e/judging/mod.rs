mod code_run;
mod lifecycle;
mod statement_counts;
mod submission;
mod testlib_checker;
mod verdicts;

pub const CPP_SUM: &str = r#"
#include <iostream>
int main() {
    int n;
    std::cin >> n;
    long long sum = 0;
    for (int i = 0; i < n; i++) {
        int x;
        std::cin >> x;
        sum += x;
    }
    std::cout << sum << std::endl;
    return 0;
}
"#;
