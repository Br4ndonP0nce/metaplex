pub mod utils;

use {
    crate::utils::{assert_initialized, assert_owned_by, spl_token_transfer, TokenTransferParams},
    anchor_lang::{
        prelude::*, solana_program::system_program, AnchorDeserialize, AnchorSerialize, Key,
    },
    arrayref::array_ref,
    spl_token::state::{Account, Mint},
    spl_token_metadata::{
        instruction::{create_master_edition, create_metadata_accounts},
        state::{
            MAX_CREATOR_LEN, MAX_CREATOR_LIMIT, MAX_NAME_LENGTH, MAX_SYMBOL_LENGTH, MAX_URI_LENGTH,
        },
    },
    std::cell::Ref,
};

const PREFIX: &str = "candy_machine";
#[program]
pub mod nft_candy_machine {
    use anchor_lang::solana_program::{
        program::{invoke, invoke_signed},
        system_instruction,
    };

    use super::*;

    pub fn mint_nft<'info>(ctx: Context<'_, '_, '_, 'info, MintNFT<'info>>) -> ProgramResult {
        let candy_machine = &mut ctx.accounts.candy_machine;
        let config = &ctx.accounts.config;
        let clock = &ctx.accounts.clock;

        match candy_machine.data.go_live_date {
            None => {
                if *ctx.accounts.payer.key != candy_machine.authority {
                    return Err(ErrorCode::CandyMachineNotLiveYet.into());
                }
            }
            Some(val) => {
                if clock.unix_timestamp < val {
                    if *ctx.accounts.payer.key != candy_machine.authority {
                        return Err(ErrorCode::CandyMachineNotLiveYet.into());
                    }
                }
            }
        }

        if candy_machine.items_redeemed >= candy_machine.data.items_available {
            return Err(ErrorCode::CandyMachineEmpty.into());
        }

        if let Some(mint) = candy_machine.token_mint {
            let token_account_info = &ctx.remaining_accounts[0];
            let transfer_authority_info = &ctx.remaining_accounts[1];
            let token_account: Account = assert_initialized(&token_account_info)?;

            assert_owned_by(&token_account_info, &spl_token::id())?;

            if token_account.mint != mint {
                return Err(ErrorCode::MintMismatch.into());
            }

            if token_account.amount < candy_machine.data.price {
                return Err(ErrorCode::NotEnoughTokens.into());
            }

            spl_token_transfer(TokenTransferParams {
                source: token_account_info.clone(),
                destination: ctx.accounts.wallet.clone(),
                authority: transfer_authority_info.clone(),
                authority_signer_seeds: &[],
                token_program: ctx.accounts.token_program.clone(),
                amount: candy_machine.data.price,
            })?;
        } else {
            if ctx.accounts.payer.lamports() < candy_machine.data.price {
                return Err(ErrorCode::NotEnoughSOL.into());
            }

            invoke(
                &system_instruction::transfer(
                    &ctx.accounts.payer.key,
                    ctx.accounts.wallet.key,
                    candy_machine.data.price,
                ),
                &[
                    ctx.accounts.payer.clone(),
                    ctx.accounts.wallet.clone(),
                    ctx.accounts.system_program.clone(),
                ],
            )?;
        }

        candy_machine.items_redeemed = candy_machine
            .items_redeemed
            .checked_add(1)
            .ok_or(ErrorCode::NumericalOverflowError)?;

        let config_line = get_config_line(&config.to_account_info(), 0)?;

        let config_key = config.key();
        let authority_seeds = [
            PREFIX.as_bytes(),
            config_key.as_ref(),
            candy_machine.data.uuid.as_bytes(),
            &[candy_machine.bump],
        ];

        let mut creators: Vec<spl_token_metadata::state::Creator> =
            vec![spl_token_metadata::state::Creator {
                address: candy_machine.key(),
                verified: true,
                share: 0,
            }];

        for c in &config.data.creators {
            creators.push(spl_token_metadata::state::Creator {
                address: c.address,
                verified: false,
                share: c.share,
            });
        }

        let mut metadata_infos = vec![
            ctx.accounts.metadata.clone(),
            ctx.accounts.mint.clone(),
            ctx.accounts.mint_authority.clone(),
            ctx.accounts.payer.clone(),
            ctx.accounts.token_metadata_program.clone(),
            ctx.accounts.token_program.clone(),
            ctx.accounts.system_program.clone(),
            ctx.accounts.rent.to_account_info().clone(),
        ];

        let mut master_edition_infos = vec![
            ctx.accounts.master_edition.clone(),
            ctx.accounts.mint.clone(),
            ctx.accounts.mint_authority.clone(),
            ctx.accounts.payer.clone(),
            ctx.accounts.metadata.clone(),
            ctx.accounts.token_metadata_program.clone(),
            ctx.accounts.token_program.clone(),
            ctx.accounts.system_program.clone(),
            ctx.accounts.rent.to_account_info().clone(),
        ];

        let mut update_authority_is_signer = true;
        let mut update_authority = candy_machine.key();
        if !ctx.accounts.config.data.retain_authority {
            update_authority_is_signer = false;
            update_authority = *ctx.accounts.update_authority.key;
            metadata_infos.push(ctx.accounts.update_authority.clone());
            master_edition_infos.push(ctx.accounts.update_authority.clone())
        } else {
            metadata_infos.push(candy_machine.to_account_info().clone());
            master_edition_infos.push(candy_machine.to_account_info().clone())
        }
        msg!("metadata {}", ctx.accounts.metadata.key);
        msg!("mint {}", ctx.accounts.mint.key);
        msg!("mint authority {}", ctx.accounts.mint_authority.key);
        msg!("payer {}", ctx.accounts.payer.key);
        msg!("update auth {}", update_authority);

        invoke_signed(
            &create_metadata_accounts(
                *ctx.accounts.token_metadata_program.key,
                *ctx.accounts.metadata.key,
                *ctx.accounts.mint.key,
                *ctx.accounts.mint_authority.key,
                *ctx.accounts.payer.key,
                update_authority,
                config_line.name,
                config.data.symbol.clone(),
                config_line.uri,
                Some(creators),
                config.data.seller_fee_basis_points,
                update_authority_is_signer,
                config.data.is_mutable,
            ),
            metadata_infos.as_slice(),
            &[&authority_seeds],
        )?;

        invoke_signed(
            &create_master_edition(
                *ctx.accounts.token_metadata_program.key,
                *ctx.accounts.master_edition.key,
                *ctx.accounts.mint.key,
                update_authority,
                *ctx.accounts.mint_authority.key,
                *ctx.accounts.metadata.key,
                *ctx.accounts.payer.key,
                Some(config.data.max_supply),
            ),
            master_edition_infos.as_slice(),
            &[&authority_seeds],
        )?;

        Ok(())
    }

    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        bump: u8,
        data: ConfigData,
    ) -> ProgramResult {
        let config = &mut ctx.accounts.config;
        config.bump = bump;
        if data.uuid.len() != 6 {
            return Err(ErrorCode::UuidMustBeExactly6Length.into());
        }
        config.authority = *ctx.accounts.authority.key;
        config.data = data;

        let mut array_of_zeroes = vec![];
        while array_of_zeroes.len() < MAX_SYMBOL_LENGTH - config.data.symbol.len() {
            array_of_zeroes.push(0u8);
        }
        let new_symbol =
            config.data.symbol.clone() + std::str::from_utf8(&array_of_zeroes).unwrap();
        config.data.symbol = new_symbol;

        // - 1 because we are going to be a creator
        if config.data.creators.len() > MAX_CREATOR_LIMIT - 1 {
            return Err(ErrorCode::TooManyCreators.into());
        }

        Ok(())
    }

    pub fn add_config_lines(
        ctx: Context<AddConfigLines>,
        index: u32,
        config_lines: Vec<ConfigLine>,
    ) -> ProgramResult {
        let config = &mut ctx.accounts.config;
        let account = config.to_account_info();
        let current_count = get_config_count(&account.data.borrow())?;
        let mut data = account.data.borrow_mut();

        let mut fixed_config_lines = vec![];

        if index > config.data.max_number_of_lines - 1 {
            return Err(ErrorCode::IndexGreaterThanLength.into());
        }

        for line in &config_lines {
            let mut array_of_zeroes = vec![];
            while array_of_zeroes.len() < MAX_NAME_LENGTH - line.name.len() {
                array_of_zeroes.push(0u8);
            }
            let name = line.name.clone() + std::str::from_utf8(&array_of_zeroes).unwrap();

            let mut array_of_zeroes = vec![];
            while array_of_zeroes.len() < MAX_URI_LENGTH - line.uri.len() {
                array_of_zeroes.push(0u8);
            }
            let uri = line.uri.clone() + std::str::from_utf8(&array_of_zeroes).unwrap();
            fixed_config_lines.push(ConfigLine { name, uri })
        }

        let as_vec = fixed_config_lines.try_to_vec()?;
        // remove unneeded u32 because we're just gonna edit the u32 at the front
        let serialized: &[u8] = &as_vec.as_slice()[4..];

        let position = CONFIG_ARRAY_START + 4 + (index as usize) * CONFIG_LINE_SIZE;

        let array_slice: &mut [u8] =
            &mut data[position..position + fixed_config_lines.len() * CONFIG_LINE_SIZE];
        array_slice.copy_from_slice(serialized);
        let new_count = current_count
            .checked_add(fixed_config_lines.len())
            .ok_or(ErrorCode::NumericalOverflowError)?;
        // plug in new count.
        data[CONFIG_ARRAY_START..CONFIG_ARRAY_START + 4]
            .copy_from_slice(&(new_count as u32).to_le_bytes());

        Ok(())
    }

    pub fn initialize_candy_machine(
        ctx: Context<InitializeCandyMachine>,
        bump: u8,
        data: CandyMachineData,
    ) -> ProgramResult {
        let candy_machine = &mut ctx.accounts.candy_machine;

        if data.uuid.len() != 6 {
            return Err(ErrorCode::UuidMustBeExactly6Length.into());
        }
        candy_machine.data = data;
        candy_machine.wallet = *ctx.accounts.wallet.key;
        candy_machine.authority = *ctx.accounts.authority.key;
        candy_machine.config = ctx.accounts.config.key();
        candy_machine.bump = bump;
        if ctx.remaining_accounts.len() > 0 {
            let token_mint_info = &ctx.remaining_accounts[0];
            let _token_mint: Mint = assert_initialized(&token_mint_info)?;
            let token_account: Account = assert_initialized(&ctx.accounts.wallet)?;

            assert_owned_by(&token_mint_info, &spl_token::id())?;
            assert_owned_by(&ctx.accounts.wallet, &spl_token::id())?;

            if token_account.mint != *token_mint_info.key {
                return Err(ErrorCode::MintMismatch.into());
            }

            candy_machine.token_mint = Some(*token_mint_info.key);
        }

        let _config_line = match get_config_line(&ctx.accounts.config.to_account_info(), 0) {
            Ok(val) => val,
            Err(_) => return Err(ErrorCode::ConfigMustHaveAtleastOneEntry.into()),
        };

        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(bump: u8, data: CandyMachineData)]
pub struct InitializeCandyMachine<'info> {
    #[account(init, seeds=[PREFIX.as_bytes(), config.key().as_ref(), data.uuid.as_bytes()], payer=payer, bump=bump, space=8+32+32+33+32+64+64+64+200)]
    candy_machine: ProgramAccount<'info, CandyMachine>,
    #[account(constraint= !wallet.data_is_empty() || wallet.lamports() > 0 )]
    wallet: AccountInfo<'info>,
    #[account(seeds=[PREFIX.as_bytes(), authority.key.as_ref(), &config.data.uuid.as_bytes(), &[config.bump]])]
    config: ProgramAccount<'info, Config>,
    #[account(signer, constraint= !authority.data_is_empty() || authority.lamports() > 0)]
    authority: AccountInfo<'info>,
    #[account(mut, signer)]
    payer: AccountInfo<'info>,
    #[account(address = system_program::ID)]
    system_program: AccountInfo<'info>,
    rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(bump: u8, data: ConfigData)]
pub struct InitializeConfig<'info> {
    #[account(init, seeds=[PREFIX.as_bytes(), authority.key.as_ref(), data.uuid.as_bytes()], payer=payer, bump=bump, space=CONFIG_ARRAY_START+4+(data.max_number_of_lines as usize)*CONFIG_LINE_SIZE)]
    config: ProgramAccount<'info, Config>,
    #[account(constraint= !authority.data_is_empty() || authority.lamports() > 0 )]
    authority: AccountInfo<'info>,
    #[account(mut, signer)]
    payer: AccountInfo<'info>,
    #[account(address = system_program::ID)]
    system_program: AccountInfo<'info>,
    rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct AddConfigLines<'info> {
    #[account(mut, seeds=[PREFIX.as_bytes(), authority.key.as_ref(), &config.data.uuid.as_bytes(), &[config.bump]])]
    config: ProgramAccount<'info, Config>,
    #[account(signer)]
    authority: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct MintNFT<'info> {
    #[account(seeds=[PREFIX.as_bytes(), config.authority.as_ref(), &config.data.uuid.as_bytes(), &[config.bump]])]
    config: ProgramAccount<'info, Config>,
    #[account(mut, seeds=[PREFIX.as_bytes(), config.key().as_ref(), candy_machine.data.uuid.as_bytes(), &[candy_machine.bump]])]
    candy_machine: ProgramAccount<'info, CandyMachine>,
    #[account(mut, signer)]
    payer: AccountInfo<'info>,
    #[account(mut, constraint=wallet.key == &candy_machine.wallet)]
    wallet: AccountInfo<'info>,
    // With the following accounts we aren't using anchor macros because they are CPI'd
    // through to token-metadata which will do all the validations we need on them.
    #[account(mut)]
    metadata: AccountInfo<'info>,
    #[account(mut)]
    mint: AccountInfo<'info>,
    #[account(signer)]
    mint_authority: AccountInfo<'info>,
    #[account(signer)]
    update_authority: AccountInfo<'info>,
    #[account(mut)]
    master_edition: AccountInfo<'info>,
    #[account(address = spl_token_metadata::id())]
    token_metadata_program: AccountInfo<'info>,
    #[account(address = spl_token::id())]
    token_program: AccountInfo<'info>,
    #[account(address = system_program::ID)]
    system_program: AccountInfo<'info>,
    rent: Sysvar<'info, Rent>,
    clock: Sysvar<'info, Clock>,
}

#[account]
#[derive(Default)]
pub struct CandyMachine {
    pub authority: Pubkey,
    pub wallet: Pubkey,
    pub token_mint: Option<Pubkey>,
    pub config: Pubkey,
    pub data: CandyMachineData,
    pub items_redeemed: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default)]
pub struct CandyMachineData {
    pub uuid: String,
    pub price: u64,
    pub items_available: u64,
    pub go_live_date: Option<i64>,
}

pub const CONFIG_ARRAY_START: usize = 1 + // bump
32 + // authority
4 + 6 + // uuid + u32 len
4 + MAX_SYMBOL_LENGTH + // u32 len + symbol
2 + // seller fee basis points
1 + 4 + MAX_CREATOR_LIMIT*MAX_CREATOR_LEN + // optional + u32 len + actual vec
8 + //max supply
1 + // is mutable
1 + // retain authority
4; // max number of lines;

#[account]
#[derive(Default)]
pub struct Config {
    pub bump: u8,
    pub authority: Pubkey,
    pub data: ConfigData,
    // there's a borsh vec u32 denoting how many actual lines of data there are currently (eventually equals max number of lines)
    // There is actually lines and lines of data after this but we explicitly never want them deserialized.
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default)]
pub struct ConfigData {
    pub uuid: String,
    /// The symbol for the asset
    pub symbol: String,
    /// Royalty basis points that goes to creators in secondary sales (0-10000)
    pub seller_fee_basis_points: u16,
    pub creators: Vec<Creator>,
    pub max_supply: u64,
    pub is_mutable: bool,
    pub retain_authority: bool,
    pub max_number_of_lines: u32,
}

pub fn get_config_count(data: &Ref<&mut [u8]>) -> core::result::Result<usize, ProgramError> {
    return Ok(u32::from_le_bytes(*array_ref![data, CONFIG_ARRAY_START, 4]) as usize);
}

pub fn get_config_line(
    a: &AccountInfo,
    index: usize,
) -> core::result::Result<ConfigLine, ProgramError> {
    let arr = a.data.borrow();

    let total = get_config_count(&arr)?;
    if index > total {
        return Err(ErrorCode::IndexGreaterThanLength.into());
    }
    let data_array = &arr[CONFIG_ARRAY_START + 4 + index * (CONFIG_LINE_SIZE)
        ..CONFIG_ARRAY_START + 4 + (index + 1) * (CONFIG_LINE_SIZE)];

    let config_line: ConfigLine = ConfigLine::try_from_slice(data_array)?;

    Ok(config_line)
}

pub const CONFIG_LINE_SIZE: usize = 4 + MAX_NAME_LENGTH + 4 + MAX_URI_LENGTH;
#[derive(AnchorSerialize, AnchorDeserialize, Debug)]
pub struct ConfigLine {
    /// The name of the asset
    pub name: String,
    /// URI pointing to JSON representing the asset
    pub uri: String,
}

// Unfortunate duplication of token metadata so that IDL picks it up.

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct Creator {
    pub address: Pubkey,
    pub verified: bool,
    // In percentages, NOT basis points ;) Watch out!
    pub share: u8,
}

#[error]
pub enum ErrorCode {
    #[msg("Account does not have correct owner!")]
    IncorrectOwner,
    #[msg("Account is not initialized!")]
    Uninitialized,
    #[msg("Mint Mismatch!")]
    MintMismatch,
    #[msg("Index greater than length!")]
    IndexGreaterThanLength,
    #[msg("Config must have atleast one entry!")]
    ConfigMustHaveAtleastOneEntry,
    #[msg("Numerical overflow error!")]
    NumericalOverflowError,
    #[msg("Can only provide up to 4 creators to candy machine (because candy machine is one)!")]
    TooManyCreators,
    #[msg("Uuid must be exactly of 6 length")]
    UuidMustBeExactly6Length,
    #[msg("Not enough tokens to pay for this minting")]
    NotEnoughTokens,
    #[msg("Not enough SOL to pay for this minting")]
    NotEnoughSOL,
    #[msg("Token transfer failed")]
    TokenTransferFailed,
    #[msg("Candy machine is empty!")]
    CandyMachineEmpty,
    #[msg("Candy machine is not live yet!")]
    CandyMachineNotLiveYet,
}