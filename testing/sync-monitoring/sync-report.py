import pandas as pd
import matplotlib.pyplot as plt
import matplotlib.patches as mpatches
import numpy as np
from datetime import datetime

# Read and prepare data
df = pd.read_csv('/data/sync-data.csv')

def fix_timestamp(ts):
    if len(str(ts)) < 12:
        return f"2026-03-20 {ts}"
    return ts

df['timestamp'] = df['timestamp'].apply(fix_timestamp)
df['timestamp'] = pd.to_datetime(df['timestamp'])

# Monitoring data is in UTC-6, convert to UTC for consistency with sync start times
# (sync start was 22:17 UTC, first monitoring at 16:21 local ≈ 22:21-22:47 UTC)
TIMEZONE_OFFSET_HOURS = 6
df['timestamp'] = df['timestamp'] + pd.Timedelta(hours=TIMEZONE_OFFSET_HOURS)

# === SYNC TIMING ===
# Actual sync start (from logs) - used for total duration calculation
REAL_T0 = pd.Timestamp('2026-03-20 22:17:29')

# Calculate relative time from REAL t0 (in hours) - for accurate elapsed time
t0 = REAL_T0
df['hours'] = (df['timestamp'] - t0).dt.total_seconds() / 3600

# Calculate sync rates
df['blocks_delta'] = df['zaino_height'].diff()
df['time_delta'] = df['timestamp'].diff().dt.total_seconds()
df['blocks_per_sec'] = df['blocks_delta'] / df['time_delta']
df['blocks_per_hour'] = df['blocks_per_sec'] * 3600
df['rate_smooth'] = df['blocks_per_hour'].rolling(window=30, min_periods=1).mean()

# Define block ranges for analysis
RANGES = {
    'Pre-Sandblast': (0, 1_700_000),
    'Sandblast': (1_700_000, 2_000_000),
    'Post-Sandblast': (2_000_000, 3_000_000),
    'Recent': (3_000_000, 4_000_000),
}

# Zcash network upgrades (mainnet activation heights)
MILESTONES = {
    'NU5': 1_687_104,
    'NU6': 2_726_400,
    'NU6.1': 3_146_400,
}
SANDBLAST_RANGE = (1_700_000, 2_000_000)

# Split into catching-up and steady-state phases
catchup_df = df[df['lag'] > 100].copy()
steady_df = df[df['lag'] <= 100].copy()

# Calculate relative hours from REAL t0
catchup_df['hours'] = (catchup_df['timestamp'] - REAL_T0).dt.total_seconds() / 3600
if len(steady_df) > 0:
    # For steady state, hours are relative to when it caught up (not REAL_T0)
    steady_t0 = steady_df['timestamp'].min()
    steady_df['hours'] = (steady_df['timestamp'] - steady_t0).dt.total_seconds() / 3600

# Assign range labels
def get_range(height):
    for name, (low, high) in RANGES.items():
        if low <= height < high:
            return name
    return 'Recent'

catchup_df['range'] = catchup_df['zaino_height'].apply(get_range)

# ============================================================
# FIGURE 1: Catching Up Phase Overview
# ============================================================
fig1, axes1 = plt.subplots(2, 1, figsize=(14, 10))
fig1.suptitle('Zaino Mainnet Sync: Catching Up Phase', fontsize=14, fontweight='bold')

# Plot 1: Progress with lag overlay
ax1 = axes1[0]
ax1_twin = ax1.twinx()

ax1.plot(catchup_df['hours'], catchup_df['zaino_height']/1e6, 'b-', linewidth=2, label='Zaino Height')
ax1.plot(catchup_df['hours'], catchup_df['zebra_height']/1e6, 'g--', linewidth=1.5, alpha=0.7, label='Zebra Height')
ax1_twin.fill_between(catchup_df['hours'], catchup_df['lag']/1e6, alpha=0.2, color='orange')
ax1_twin.plot(catchup_df['hours'], catchup_df['lag']/1e6, 'orange', linewidth=1, alpha=0.7, label='Lag')

# Add sandblast range markers
sandblast_mask = (catchup_df['zaino_height'] >= 1.7e6) & (catchup_df['zaino_height'] <= 2e6)
if sandblast_mask.any():
    start_hr = catchup_df.loc[sandblast_mask, 'hours'].min()
    end_hr = catchup_df.loc[sandblast_mask, 'hours'].max()
    ax1.axvspan(start_hr, end_hr, alpha=0.15, color='red', label='Sandblast Range')

ax1.set_ylabel('Block Height (millions)', color='blue')
ax1_twin.set_ylabel('Lag (millions)', color='orange')
ax1.set_xlabel('Hours from start')
ax1.set_title('Sync Progress: Zaino Catching Up to Zebra')
ax1.legend(loc='upper left')
ax1_twin.legend(loc='upper right')
ax1.grid(True, alpha=0.3)

# Plot 2: Sync rate over time
ax2 = axes1[1]
valid_rate = catchup_df['rate_smooth'].replace([np.inf, -np.inf], np.nan).dropna()
ax2.plot(catchup_df['hours'], catchup_df['rate_smooth']/1e3, 'purple', linewidth=1.5)
ax2.axhline(y=valid_rate.mean()/1e3, color='red', linestyle='--', alpha=0.7,
            label=f'Avg: {valid_rate.mean()/1e3:.0f}k/hr')

# Sandblast shading
if sandblast_mask.any():
    ax2.axvspan(start_hr, end_hr, alpha=0.15, color='red', label='Sandblast Range')

ax2.set_ylabel('Sync Rate (k blocks/hour)')
ax2.set_xlabel('Hours from start')
ax2.set_title('Sync Rate Over Time (30-point rolling avg)')
ax2.legend(loc='upper right')
ax2.grid(True, alpha=0.3)
ax2.set_ylim(bottom=0)

plt.tight_layout()
fig1.savefig('/data/01-sync-overview.png', dpi=150, bbox_inches='tight')
print("Saved: /data/01-sync-overview.png")

# ============================================================
# FIGURE 2: Side-by-Side Rate Comparison (Time vs Block Height)
# ============================================================
fig2, (ax_time, ax_block) = plt.subplots(1, 2, figsize=(16, 6), sharey=True)
fig2.suptitle('Sync Rate Analysis: Time vs Block Height Dimension', fontsize=14, fontweight='bold')

# Left: Rate vs Time
ax_time.plot(catchup_df['hours'], catchup_df['rate_smooth']/1e3, 'purple', linewidth=1.2)
ax_time.axhline(y=valid_rate.mean()/1e3, color='red', linestyle='--', alpha=0.7)
if sandblast_mask.any():
    ax_time.axvspan(start_hr, end_hr, alpha=0.2, color='red', label='Sandblast')

# Add milestone markers on time axis (when each height was reached)
for name, height in MILESTONES.items():
    # Find when this height was first reached
    reached = catchup_df[catchup_df['zaino_height'] >= height]
    if len(reached) > 0:
        milestone_hr = reached['hours'].iloc[0]
        ax_time.axvline(x=milestone_hr, color='green', linestyle='--', alpha=0.8, linewidth=1.5)
        ax_time.text(milestone_hr, ax_time.get_ylim()[1]*0.95, f' {name}',
                    fontsize=8, color='green', ha='left', va='top', rotation=90)

ax_time.set_ylabel('Sync Rate (k blocks/hour)')
ax_time.set_xlabel('Hours from start')
ax_time.set_title('Rate vs Time\n(shows time to reach each milestone)')
ax_time.grid(True, alpha=0.3)
ax_time.set_ylim(bottom=0)
ax_time.legend()

# Right: Rate vs Block Height
ax_block.plot(catchup_df['zaino_height']/1e6, catchup_df['rate_smooth']/1e3, 'purple', linewidth=1.2)
ax_block.axhline(y=valid_rate.mean()/1e3, color='red', linestyle='--', alpha=0.7)
ax_block.axvspan(1.7, 2.0, alpha=0.2, color='red', label='Sandblast')

# Add network upgrade markers
height_min, height_max = catchup_df['zaino_height'].min(), catchup_df['zaino_height'].max()
for name, height in MILESTONES.items():
    if height_min <= height <= height_max:
        ax_block.axvline(x=height/1e6, color='green', linestyle='--', alpha=0.8, linewidth=1.5)
        ax_block.text(height/1e6, ax_block.get_ylim()[1]*0.95, f' {name}',
                     fontsize=8, color='green', ha='left', va='top', rotation=90)

ax_block.set_xlabel('Block Height (millions)')
ax_block.set_title('Rate vs Block Height\n(shows exact speed at each height)')
ax_block.grid(True, alpha=0.3)
ax_block.legend(loc='upper right')

plt.tight_layout()
fig2.savefig('/data/02-rate-comparison.png', dpi=150, bbox_inches='tight')
print("Saved: /data/02-rate-comparison.png")

# ============================================================
# FIGURE 3: Box Plot by Range
# ============================================================
fig3, ax3 = plt.subplots(figsize=(10, 6))
fig3.suptitle('Sync Rate Distribution by Block Range', fontsize=14, fontweight='bold')

# Prepare data for box plot
range_data = []
range_labels = []
range_colors = ['#2ecc71', '#e74c3c', '#3498db', '#9b59b6']

for i, (name, (low, high)) in enumerate(RANGES.items()):
    mask = (catchup_df['zaino_height'] >= low) & (catchup_df['zaino_height'] < high)
    rates = catchup_df.loc[mask, 'rate_smooth'].replace([np.inf, -np.inf], np.nan).dropna() / 1e3
    if len(rates) > 0:
        range_data.append(rates)
        range_labels.append(f'{name}\n({low/1e6:.1f}M-{high/1e6:.1f}M)')

bp = ax3.boxplot(range_data, labels=range_labels, patch_artist=True)
for patch, color in zip(bp['boxes'], range_colors[:len(range_data)]):
    patch.set_facecolor(color)
    patch.set_alpha(0.6)

ax3.set_ylabel('Sync Rate (k blocks/hour)')
ax3.set_xlabel('Block Range')
ax3.grid(True, alpha=0.3, axis='y')

# Highlight sandblast
for i, label in enumerate(range_labels):
    if 'Sandblast' in label:
        bp['boxes'][i].set_edgecolor('red')
        bp['boxes'][i].set_linewidth(2)

plt.tight_layout()
fig3.savefig('/data/03-rate-boxplot.png', dpi=150, bbox_inches='tight')
print("Saved: /data/03-rate-boxplot.png")

# ============================================================
# FIGURE 4: Steady State (After Catch-up)
# ============================================================
if len(steady_df) > 10:
    fig4, ax4 = plt.subplots(figsize=(12, 5))
    fig4.suptitle('Steady State: Zaino Keeping Pace with Zebra', fontsize=14, fontweight='bold')

    ax4.plot(steady_df['hours'], steady_df['zaino_height'], 'b-', linewidth=2, label='Zaino')
    ax4.plot(steady_df['hours'], steady_df['zebra_height'], 'g--', linewidth=1.5, alpha=0.7, label='Zebra')

    ax4.set_ylabel('Block Height')
    ax4.set_xlabel('Hours since catch-up')
    ax4.set_title(f'Post-Sync: {len(steady_df)} data points, Lag = 0')
    ax4.legend()
    ax4.grid(True, alpha=0.3)

    plt.tight_layout()
    fig4.savefig('/data/04-steady-state.png', dpi=150, bbox_inches='tight')
    print("Saved: /data/04-steady-state.png")

# ============================================================
# FIGURE 5: Statistics Tables (rendered as image)
# ============================================================

# Calculate stats
total_duration = catchup_df['timestamp'].max() - REAL_T0
monitored_duration = catchup_df['timestamp'].max() - catchup_df['timestamp'].min()
blocks_synced = catchup_df['zaino_height'].max()

# Per-range stats
range_stats = []
for name, (low, high) in RANGES.items():
    mask = (catchup_df['zaino_height'] >= low) & (catchup_df['zaino_height'] < high)
    subset = catchup_df[mask]
    if len(subset) > 1:
        rates = subset['rate_smooth'].replace([np.inf, -np.inf], np.nan).dropna()
        duration = subset['timestamp'].max() - subset['timestamp'].min()
        blocks = subset['zaino_height'].max() - subset['zaino_height'].min()
        avg_rate = rates.mean() / 1e3
        std_rate = rates.std() / 1e3
        time_pct = duration.total_seconds() / monitored_duration.total_seconds() * 100
        range_stats.append({
            'name': name, 'low': low, 'high': high, 'blocks': blocks,
            'duration': duration, 'avg_rate': avg_rate, 'std_rate': std_rate, 'time_pct': time_pct
        })

fig5, axes5 = plt.subplots(2, 1, figsize=(12, 8))
fig5.suptitle('Zaino Mainnet Sync Report', fontsize=16, fontweight='bold', y=0.98)

# Table 1: Overall Statistics
ax_overall = axes5[0]
ax_overall.axis('off')
overall_data = [
    ['Sync Start (UTC)', str(REAL_T0)[:19]],
    ['Total Duration', str(total_duration).split('.')[0]],
    ['Monitored Duration', str(monitored_duration).split('.')[0]],
    ['Blocks Synced', f'{blocks_synced:,}'],
    ['Average Rate', f'{valid_rate.mean():,.0f} blocks/hr'],
    ['Peak Rate', f'{valid_rate.max():,.0f} blocks/hr'],
    ['Min Rate', f'{valid_rate.min():,.0f} blocks/hr'],
]
table1 = ax_overall.table(cellText=overall_data, colLabels=['Metric', 'Value'],
                          loc='center', cellLoc='left', colWidths=[0.4, 0.4])
table1.auto_set_font_size(False)
table1.set_fontsize(11)
table1.scale(1.2, 1.8)
for key, cell in table1.get_celld().items():
    if key[0] == 0:  # Header row
        cell.set_facecolor('#4472C4')
        cell.set_text_props(color='white', fontweight='bold')
    else:
        cell.set_facecolor('#D6DCE4' if key[0] % 2 == 0 else 'white')
ax_overall.set_title('Overall Statistics', fontsize=12, fontweight='bold', pad=10)

# Table 2: Per-Range Statistics
ax_range = axes5[1]
ax_range.axis('off')
range_data = []
for s in range_stats:
    dur_str = str(s['duration']).split('.')[0].replace('0 days ', '')
    range_data.append([
        s['name'],
        f"{s['low']/1e6:.1f}M - {s['high']/1e6:.1f}M",
        f"{s['blocks']:,}",
        dur_str,
        f"{s['avg_rate']:.0f}k",
        f"{s['std_rate']:.0f}k",
        f"{s['time_pct']:.1f}%"
    ])
table2 = ax_range.table(cellText=range_data,
                        colLabels=['Range', 'Heights', 'Blocks', 'Duration', 'Avg Rate', 'Std Dev', 'Time %'],
                        loc='center', cellLoc='center', colWidths=[0.18, 0.18, 0.12, 0.14, 0.12, 0.12, 0.1])
table2.auto_set_font_size(False)
table2.set_fontsize(10)
table2.scale(1.2, 1.8)
for key, cell in table2.get_celld().items():
    if key[0] == 0:  # Header
        cell.set_facecolor('#4472C4')
        cell.set_text_props(color='white', fontweight='bold')
    elif key[0] == 2:  # Sandblast row (index 2 = row 1 in data, but +1 for header)
        cell.set_facecolor('#FFCCCC')
    else:
        cell.set_facecolor('#D6DCE4' if key[0] % 2 == 0 else 'white')
ax_range.set_title('Statistics by Block Range', fontsize=12, fontweight='bold', pad=10)

plt.tight_layout()
fig5.savefig('/data/05-stats-tables.png', dpi=150, bbox_inches='tight', facecolor='white')
print("Saved: /data/05-stats-tables.png")

# ============================================================
# FIGURE 6: Sandblast Impact Summary
# ============================================================
sandblast_stats = next((s for s in range_stats if s['name'] == 'Sandblast'), None)
if sandblast_stats:
    fig6, ax6 = plt.subplots(figsize=(10, 5))
    ax6.axis('off')
    fig6.suptitle('Sandblast Impact Analysis (1.7M - 2.0M blocks)', fontsize=14, fontweight='bold')

    overall_avg = valid_rate.mean() / 1e3
    slowdown = (overall_avg - sandblast_stats['avg_rate']) / overall_avg * 100

    impact_data = [
        ['Time Impact', f"{sandblast_stats['time_pct']:.1f}% of total sync time"],
        ['Block Range', '300,000 blocks (9% of chain)'],
        ['Average Rate', f"{sandblast_stats['avg_rate']:.0f}k blocks/hr"],
        ['Overall Average', f"{overall_avg:.0f}k blocks/hr"],
        ['Slowdown Factor', f"{slowdown:.0f}% slower than average"],
        ['Rate Variance', f"±{sandblast_stats['std_rate']:.0f}k blocks/hr"],
    ]

    table6 = ax6.table(cellText=impact_data, colLabels=['Metric', 'Value'],
                       loc='center', cellLoc='left', colWidths=[0.35, 0.45])
    table6.auto_set_font_size(False)
    table6.set_fontsize(12)
    table6.scale(1.3, 2.2)
    for key, cell in table6.get_celld().items():
        if key[0] == 0:
            cell.set_facecolor('#C00000')
            cell.set_text_props(color='white', fontweight='bold')
        else:
            cell.set_facecolor('#FFDDDD' if key[0] % 2 == 0 else '#FFE5E5')

    plt.tight_layout()
    fig6.savefig('/data/06-sandblast-impact.png', dpi=150, bbox_inches='tight', facecolor='white')
    print("Saved: /data/06-sandblast-impact.png")

print("\nGenerated all report images.")
