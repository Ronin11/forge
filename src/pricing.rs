//! Price a claude attempt at its API list from the CLI's own token counts
//! (docs/ECONOMIST.md, "Price every provider at its API list").

use crate::agent::{Outcome, Provider, Runner};

/// USD per million tokens. Cache reads default to a tenth of the input
/// price; cache creation is charged at the input price.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Tokens {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

impl Prices {
    /// The prices a claude provider sets; `None` for another runner or when
    /// no input/output price is set (the CLI's own figure stands).
    pub fn for_claude(p: &Provider) -> Option<Prices> {
        if p.runner != Runner::ClaudeCli
            || (p.price_input_per_million <= 0.0 && p.price_output_per_million <= 0.0)
        {
            return None;
        }
        Some(Prices {
            input: p.price_input_per_million,
            output: p.price_output_per_million,
            cache_read: p.price_cache_read_per_million,
        })
    }

    pub fn cost(&self, t: Tokens) -> f64 {
        let cache_read = self.cache_read.unwrap_or(self.input / 10.0);
        (t.input as f64 * self.input
            + t.output as f64 * self.output
            + t.cache_read as f64 * cache_read
            + t.cache_creation as f64 * self.input)
            / 1_000_000.0
    }
}

/// Replace the CLI's cost with the list-price one, keeping the CLI's figure
/// in `cli_cost_usd`. A run that reported no tokens keeps the CLI's figure.
pub fn price_outcome(out: &mut Outcome, prices: &Prices) {
    if out.input_tokens.is_none() && out.output_tokens.is_none() {
        return;
    }
    out.cli_cost_usd = out.cost_usd;
    out.cost_usd = Some(prices.cost(Tokens {
        input: out.input_tokens.unwrap_or(0),
        output: out.output_tokens.unwrap_or(0),
        cache_read: out.cache_read_input_tokens.unwrap_or(0),
        cache_creation: out.cache_creation_input_tokens.unwrap_or(0),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prices(cache_read: Option<f64>) -> Prices {
        Prices {
            input: 4.0,
            output: 20.0,
            cache_read,
        }
    }

    #[test]
    fn cache_reads_default_to_a_tenth_of_input_and_creation_is_at_input() {
        let t = Tokens {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_creation: 250_000,
        };
        // 4 + 10 + 2_000_000*0.4/1e6 + 1
        let want = 4.0 + 10.0 + 0.8 + 1.0;
        assert!((prices(None).cost(t) - want).abs() < 1e-9);
    }

    #[test]
    fn an_explicit_cache_read_price_wins() {
        let t = Tokens {
            cache_read: 1_000_000,
            ..Tokens::default()
        };
        assert!((prices(Some(0.5)).cost(t) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn price_outcome_keeps_the_cli_figure_beside_the_computed_one() {
        let mut out = Outcome {
            cost_usd: Some(9.99),
            input_tokens: Some(1000),
            output_tokens: Some(100),
            ..Outcome::default()
        };
        price_outcome(&mut out, &prices(None));
        assert_eq!(out.cli_cost_usd, Some(9.99));
        assert!((out.cost_usd.unwrap() - 0.006).abs() < 1e-9);
    }

    #[test]
    fn a_run_with_no_tokens_keeps_the_cli_figure() {
        let mut out = Outcome {
            cost_usd: Some(1.0),
            ..Outcome::default()
        };
        price_outcome(&mut out, &prices(None));
        assert_eq!(out.cost_usd, Some(1.0));
        assert_eq!(out.cli_cost_usd, None);
    }

    #[test]
    fn only_a_priced_claude_provider_has_prices() {
        let mut p = Provider::default();
        assert_eq!(Prices::for_claude(&p), None);
        p.price_input_per_million = 2.0;
        p.price_output_per_million = 10.0;
        assert_eq!(
            Prices::for_claude(&p),
            Some(Prices {
                input: 2.0,
                output: 10.0,
                cache_read: None
            })
        );
        p.runner = Runner::CodexCli;
        assert_eq!(Prices::for_claude(&p), None);
    }
}
