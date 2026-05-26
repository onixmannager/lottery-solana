use anchor_lang::prelude::*;

declare_id!("8HjujGXK2K1hj4ogKhGa5ez2mvsaapgqU9kWLubnRJTS");

const MAX_PLAYERS_PER_ROUND: usize = 2000;

#[program]
pub mod lottery {
    use super::*;

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        let global = &mut ctx.accounts.global;
        global.admin = ctx.accounts.payer.key();
        global.round_count = 0;
        Ok(())
    }

    pub fn create_round(
        ctx: Context<CreateRound>,
        ticket_price: u64,
        start_time: i64,
        end_time: i64,
        max_tickets: u64,
    ) -> Result<()> {
        let global = &mut ctx.accounts.global;
        require_keys_eq!(global.admin, ctx.accounts.admin.key(), LotteryError::Unauthorized);

        let round = &mut ctx.accounts.round;
        round.id = global.round_count;
        round.ticket_price = ticket_price;
        round.start_time = start_time;
        round.end_time = end_time;
        round.max_tickets = max_tickets;
        round.player_count = 0;
        round.total_prize = 0;
        round.winner = Pubkey::default();
        round.is_finalized = false;
        round.admin = ctx.accounts.admin.key();
        round.bump = ctx.bumps.round;

        global.round_count += 1;
        Ok(())
    }

    pub fn buy_ticket(ctx: Context<BuyTicket>, round_id: u64) -> Result<()> {
        let round = &mut ctx.accounts.round;
        let clock = Clock::get()?;

        require!(!round.is_finalized, LotteryError::RoundAlreadyFinalized);
        require!(
            clock.unix_timestamp >= round.start_time && clock.unix_timestamp <= round.end_time,
            LotteryError::RoundNotActive
        );
        require!(round.player_count < round.max_tickets, LotteryError::RoundFull);

        let cpi_context = CpiContext::new(
            ctx.accounts.system_program.to_account_info(),
            anchor_lang::system_program::Transfer {
                from: ctx.accounts.buyer.to_account_info(),
                to: round.to_account_info(),
            },
        );
        anchor_lang::system_program::transfer(cpi_context, round.ticket_price)?;

        let idx = round.player_count as usize;
        round.players[idx] = ctx.accounts.buyer.key();
        round.player_count += 1;
        round.total_prize += round.ticket_price;

        let ticket = &mut ctx.accounts.ticket;
        ticket.owner = ctx.accounts.buyer.key();
        ticket.round_id = round_id;
        ticket.ticket_number = round.player_count;

        Ok(())
    }

    // ─── Finaliza Y paga al ganador en una sola transacción ────────────────
    // El admin pasa la cuenta `winner` que debe coincidir con players[slot % player_count].
    // El programa verifica internamente — si no coincide, revierte.
    // Igual que pick_winner en el contrato anterior: un solo paso, pago inmediato.
    pub fn finalize_round(ctx: Context<FinalizeRound>, _round_id: u64) -> Result<()> {
        let prize: u64;
        {
            let round = &mut ctx.accounts.round;
            require_keys_eq!(round.admin, ctx.accounts.admin.key(), LotteryError::Unauthorized);

            let clock = Clock::get()?;
            require!(!round.is_finalized, LotteryError::RoundAlreadyFinalized);
            require!(clock.unix_timestamp >= round.end_time, LotteryError::RoundStillActive);
            require!(round.player_count > 0, LotteryError::NoTickets);

            // Sortear ganador
            let winner_index = (clock.slot % round.player_count) as usize;
            let winner_pubkey = round.players[winner_index];

            // Verificar que la cuenta pasada coincide con el ganador sorteado
            require_keys_eq!(
                winner_pubkey,
                ctx.accounts.winner.key(),
                LotteryError::WrongWinnerAccount
            );

            round.winner = winner_pubkey;
            round.is_finalized = true;
            prize = round.total_prize;
            round.total_prize = 0;
            // préstamo mutable se suelta aquí
        }

        // Pago inmediato al ganador — igual que pick_winner del contrato viejo
        ctx.accounts.round.sub_lamports(prize)?;
        **ctx.accounts.winner.to_account_info().try_borrow_mut_lamports()? += prize;

        Ok(())
    }

    // ─── claim_prize: respaldo por si finalize_round pasó la cuenta incorrecta ───
    // Si el slot cambió entre el cálculo frontend y la ejecución on-chain,
    // finalize_round habrá revertido. En ese caso el admin puede usar emergency_finalize,
    // o si logró finalizar pero sin pagar (no debería ocurrir), este claim sirve de red.
    pub fn claim_prize(ctx: Context<ClaimPrize>, _round_id: u64) -> Result<()> {
        let round = &mut ctx.accounts.round;
        require!(round.is_finalized, LotteryError::RoundNotFinalized);
        require_keys_eq!(round.winner, ctx.accounts.winner.key(), LotteryError::NotWinner);
        require!(round.total_prize > 0, LotteryError::NoPrize);

        let prize = round.total_prize;
        round.total_prize = 0;
        round.sub_lamports(prize)?;
        **ctx.accounts.winner.to_account_info().try_borrow_mut_lamports()? += prize;
        Ok(())
    }

    // ─── emergency_finalize: el admin elige manualmente al ganador y paga ───
    pub fn emergency_finalize(
        ctx: Context<EmergencyFinalize>,
        _round_id: u64,
        winner_override: Pubkey,
    ) -> Result<()> {
        let round = &mut ctx.accounts.round;
        require_keys_eq!(round.admin, ctx.accounts.admin.key(), LotteryError::Unauthorized);
        require!(!round.is_finalized, LotteryError::RoundAlreadyFinalized);

        round.winner = winner_override;
        round.is_finalized = true;

        let prize = round.total_prize;
        round.total_prize = 0;
        round.sub_lamports(prize)?;
        **ctx.accounts.winner_override.to_account_info().try_borrow_mut_lamports()? += prize;
        Ok(())
    }
}

// ─── CUENTAS ────────────────────────────────────────────────

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = payer,
        space = 8 + std::mem::size_of::<LotteryGlobal>(),
        seeds = [b"global"],
        bump
    )]
    pub global: Account<'info, LotteryGlobal>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateRound<'info> {
    #[account(
        mut,
        seeds = [b"global"],
        bump
    )]
    pub global: Account<'info, LotteryGlobal>,
    #[account(
        init,
        payer = admin,
        space = 8 + std::mem::size_of::<Round>(),
        seeds = [b"round", global.round_count.to_le_bytes().as_ref()],
        bump
    )]
    pub round: Account<'info, Round>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct BuyTicket<'info> {
    #[account(
        mut,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        init,
        payer = buyer,
        space = 8 + std::mem::size_of::<Ticket>(),
        seeds = [b"ticket", round_id.to_le_bytes().as_ref(), (round.player_count + 1).to_le_bytes().as_ref()],
        bump
    )]
    pub ticket: Account<'info, Ticket>,
    #[account(mut)]
    pub buyer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

// FinalizeRound ahora incluye winner — el admin la pasa, el programa verifica
#[derive(Accounts)]
#[instruction(_round_id: u64)]
pub struct FinalizeRound<'info> {
    #[account(
        mut,
        seeds = [b"round", _round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: El programa verifica que coincide con players[slot % player_count]
    #[account(mut)]
    pub winner: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(_round_id: u64)]
pub struct ClaimPrize<'info> {
    #[account(
        mut,
        seeds = [b"round", _round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    /// CHECK: La dirección debe ser round.winner
    #[account(mut)]
    pub winner: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(_round_id: u64, winner_override: Pubkey)]
pub struct EmergencyFinalize<'info> {
    #[account(
        mut,
        seeds = [b"round", _round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: dirección proporcionada por el admin
    #[account(mut)]
    pub winner_override: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

// ─── ESTRUCTURAS DE DATOS ───────────────────────────────────

#[account]
pub struct LotteryGlobal {
    pub admin: Pubkey,
    pub round_count: u64,
}

#[account]
pub struct Round {
    pub id: u64,
    pub ticket_price: u64,
    pub start_time: i64,
    pub end_time: i64,
    pub max_tickets: u64,
    pub players: [Pubkey; MAX_PLAYERS_PER_ROUND],
    pub player_count: u64,
    pub winner: Pubkey,
    pub is_finalized: bool,
    pub total_prize: u64,
    pub admin: Pubkey,
    pub bump: u8,
}

#[account]
pub struct Ticket {
    pub owner: Pubkey,
    pub round_id: u64,
    pub ticket_number: u64,
}

// ─── ERRORES ────────────────────────────────────────────────

#[error_code]
pub enum LotteryError {
    #[msg("No autorizado")]
    Unauthorized,
    #[msg("Ronda ya finalizada")]
    RoundAlreadyFinalized,
    #[msg("Ronda no activa (fuera del tiempo permitido)")]
    RoundNotActive,
    #[msg("Ronda llena (máximo de boletos alcanzado)")]
    RoundFull,
    #[msg("La ronda aún está activa (no se puede finalizar antes de tiempo)")]
    RoundStillActive,
    #[msg("No hay boletos vendidos en esta ronda")]
    NoTickets,
    #[msg("La ronda no ha sido finalizada aún")]
    RoundNotFinalized,
    #[msg("Solo el ganador puede reclamar")]
    NotWinner,
    #[msg("No hay premio acumulado")]
    NoPrize,
    #[msg("La cuenta winner no coincide con el ganador sorteado")]
    WrongWinnerAccount,
}
