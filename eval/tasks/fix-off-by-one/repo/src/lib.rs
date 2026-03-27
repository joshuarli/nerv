/// Sum integers from 1 to n (inclusive).
pub fn sum_to(n: u32) -> u32 {
    let mut total = 0;
    for i in 1..n {  // BUG: should be 1..=n
        total += i;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_to_5() {
        assert_eq!(sum_to(5), 15);
    }

    #[test]
    fn sum_to_1() {
        assert_eq!(sum_to(1), 1);
    }

    #[test]
    fn sum_to_100() {
        assert_eq!(sum_to(100), 5050);
    }
}
