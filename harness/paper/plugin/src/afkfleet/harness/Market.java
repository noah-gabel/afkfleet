package afkfleet.harness;

import java.math.BigDecimal;
import java.math.RoundingMode;
import java.text.DecimalFormat;
import java.text.DecimalFormatSymbols;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Optional;
import java.util.regex.Pattern;

import io.papermc.paper.datacomponent.DataComponentTypes;
import io.papermc.paper.datacomponent.item.PotionContents;
import org.bukkit.inventory.ItemStack;
import org.bukkit.potion.PotionType;

/**
 * The harness's market: balances, auction listings, expired listings and the sell menu's prices.
 * Everything runs on the server's main thread, so a listing is bought at most once: the purchase
 * checks and removes it in one step.
 */
final class Market {
    /** A listing in the auction house. */
    record Listing(int id, String seller, ItemStack item, BigDecimal price, long expiresAtTick) {}

    /** Why a purchase didn't happen. */
    enum Refusal {
        GONE("This listing is no longer available."),
        OWN("You can't buy your own listing."),
        POOR("You can't afford this."),
        FULL("Your inventory is full.");

        final String text;

        Refusal(String text) {
            this.text = text;
        }
    }

    /** The highest price a listing or a payment takes. */
    static final BigDecimal MAX = new BigDecimal("1000000000");
    /** A price as a player types it: digits, optional thousands commas, up to two decimals. */
    private static final Pattern PRICE = Pattern.compile("(\\d{1,3}(,\\d{3})+|\\d+)(\\.\\d{1,2})?");

    private final Map<Integer, Listing> listings = new LinkedHashMap<>();
    private final Map<String, List<ItemStack>> expired = new HashMap<>();
    private final Map<String, BigDecimal> balances = new HashMap<>();
    private final Map<String, BigDecimal> prices = new HashMap<>();
    private int nextId = 1;
    private long expiryTicks = 20L * 3600;

    Market() {
        defaultPrices();
    }

    private void defaultPrices() {
        prices.clear();
        prices.put("minecraft:potion:turtle_master", new BigDecimal("250"));
        prices.put("minecraft:potion:long_turtle_master", new BigDecimal("300"));
        prices.put("minecraft:potion:strong_turtle_master", new BigDecimal("350"));
        prices.put("minecraft:wheat", new BigDecimal("2"));
    }

    /** Clears every listing, expired item, balance and price, and the expiry back to an hour. */
    void reset() {
        listings.clear();
        expired.clear();
        balances.clear();
        defaultPrices();
        nextId = 1;
        expiryTicks = 20L * 3600;
    }

    // --- Money ---------------------------------------------------------------------------------

    /** `amount` as the server writes money: "$1,234.50". */
    static String money(BigDecimal amount) {
        DecimalFormat format = new DecimalFormat("#,##0.00", DecimalFormatSymbols.getInstance(Locale.ROOT));
        return "$" + format.format(amount.setScale(2, RoundingMode.DOWN));
    }

    /** A price a player typed, or nothing when it isn't one: above 0 and at most {@link #MAX}. */
    static Optional<BigDecimal> parsePrice(String text) {
        if (!PRICE.matcher(text).matches()) {
            return Optional.empty();
        }
        BigDecimal price = new BigDecimal(text.replace(",", ""));
        if (price.signum() <= 0 || price.compareTo(MAX) > 0) {
            return Optional.empty();
        }
        return Optional.of(price);
    }

    BigDecimal balance(String player) {
        return balances.getOrDefault(key(player), BigDecimal.ZERO);
    }

    void setBalance(String player, BigDecimal amount) {
        balances.put(key(player), amount);
    }

    /** Moves `amount` from `from` to `to`; false when `from` has too little. */
    boolean pay(String from, String to, BigDecimal amount) {
        if (balance(from).compareTo(amount) < 0) {
            return false;
        }
        setBalance(from, balance(from).subtract(amount));
        setBalance(to, balance(to).add(amount));
        return true;
    }

    // --- The auction house ---------------------------------------------------------------------

    void setExpiryTicks(long ticks) {
        expiryTicks = ticks;
    }

    Listing list(String seller, ItemStack item, BigDecimal price, long nowTick) {
        Listing listing = new Listing(nextId++, seller, item.clone(), price, nowTick + expiryTicks);
        listings.put(listing.id(), listing);
        return listing;
    }

    /** The listings, oldest first. */
    List<Listing> listings() {
        return new ArrayList<>(listings.values());
    }

    Optional<Listing> listing(int id) {
        return Optional.ofNullable(listings.get(id));
    }

    /**
     * Buys listing `id` for `buyer`, who has room for `fits` of its item: the money moves, the
     * listing goes. The caller hands the item over on success.
     */
    Optional<Refusal> buy(String buyer, int id, boolean fits) {
        Listing listing = listings.get(id);
        if (listing == null) {
            return Optional.of(Refusal.GONE);
        }
        if (key(listing.seller()).equals(key(buyer))) {
            return Optional.of(Refusal.OWN);
        }
        if (!fits) {
            return Optional.of(Refusal.FULL);
        }
        if (!pay(buyer, listing.seller(), listing.price())) {
            return Optional.of(Refusal.POOR);
        }
        listings.remove(id);
        return Optional.empty();
    }

    /** Moves listing `id` to its seller's expired items; false when there's none. */
    boolean expire(int id) {
        Listing listing = listings.remove(id);
        if (listing == null) {
            return false;
        }
        expired.computeIfAbsent(key(listing.seller()), seller -> new ArrayList<>()).add(listing.item());
        return true;
    }

    /** Expires every listing due by `nowTick`; returns how many. */
    int expireDue(long nowTick) {
        int count = 0;
        Iterator<Listing> due = listings.values().iterator();
        while (due.hasNext()) {
            Listing listing = due.next();
            if (listing.expiresAtTick() <= nowTick) {
                due.remove();
                expired.computeIfAbsent(key(listing.seller()), seller -> new ArrayList<>()).add(listing.item());
                count++;
            }
        }
        return count;
    }

    /** The seller's expired items, oldest first; the list itself, so a collection removes from it. */
    List<ItemStack> expired(String seller) {
        return expired.computeIfAbsent(key(seller), name -> new ArrayList<>());
    }

    // --- The sell menu -------------------------------------------------------------------------

    void setPrice(String itemKey, BigDecimal price) {
        prices.put(itemKey, price);
    }

    /** What one of `item` sells for, if it sells at all. */
    Optional<BigDecimal> sellPrice(ItemStack item) {
        return Optional.ofNullable(prices.get(sellKey(item)));
    }

    /** An item's key in the price table: its ID, and a potion's kind after it. */
    static String sellKey(ItemStack item) {
        String id = item.getType().getKey().toString();
        PotionContents contents = item.getData(DataComponentTypes.POTION_CONTENTS);
        if (contents != null) {
            PotionType potion = contents.potion();
            if (potion != null) {
                return id + ":" + potion.getKey().getKey();
            }
        }
        return id;
    }

    private static String key(String player) {
        return player.toLowerCase(Locale.ROOT);
    }
}
