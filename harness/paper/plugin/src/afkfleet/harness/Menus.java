package afkfleet.harness;

import java.math.BigDecimal;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Optional;

import net.kyori.adventure.text.Component;
import net.kyori.adventure.text.format.NamedTextColor;
import net.kyori.adventure.text.format.TextDecoration;
import org.bukkit.Bukkit;
import org.bukkit.Material;
import org.bukkit.entity.Player;
import org.bukkit.event.inventory.InventoryClickEvent;
import org.bukkit.event.inventory.InventoryCloseEvent;
import org.bukkit.event.inventory.InventoryDragEvent;
import org.bukkit.inventory.Inventory;
import org.bukkit.inventory.InventoryHolder;
import org.bukkit.inventory.ItemStack;
import org.bukkit.inventory.meta.ItemMeta;

/**
 * The harness's menus, each a chest screen the plugin owns, as a server's auction and sell
 * plugins build them:
 *
 * <ul>
 *   <li><b>Auction House</b> (54 slots): listings in slots 0 to 44, each the item with its price,
 *       seller and time left added to its lore; previous page (45), the page (48), expired
 *       listings (49), refresh (50) and next page (53). A page turns and a refresh redraw the
 *       same screen; a listing opens Confirm Purchase on the next tick.
 *   <li><b>Confirm Purchase</b> and <b>Confirm Listing</b> (27 slots): confirm (11), the item
 *       (13), cancel (15).
 *   <li><b>Expired Listings</b> (54 slots): the player's expired items in 0 to 44, back (49); a
 *       click collects an item into the inventory and redraws the screen.
 *   <li><b>Sell Items</b> (54 slots): slots 0 to 44 take any item, shift-clicks included; sell
 *       (49) sells what has a price and leaves the rest; closing gives back what's left.
 * </ul>
 *
 * Every click on a button is cancelled, as a plugin's are, and the server puts the item back.
 */
final class Menus {
    /** The auction house's listings per page. */
    static final int PAGE = 45;

    private final HarnessPlugin plugin;
    private final Market market;

    Menus(HarnessPlugin plugin, Market market) {
        this.plugin = plugin;
        this.market = market;
    }

    /** What a screen of the harness is, kept with its inventory. */
    abstract static class Screen implements InventoryHolder {
        Inventory inventory;

        @Override
        public Inventory getInventory() {
            return inventory;
        }
    }

    static final class AuctionScreen extends Screen {
        int page;
        /** The listing in each slot as drawn. */
        final Map<Integer, Integer> listingAt = new java.util.HashMap<>();
    }

    static final class ConfirmPurchase extends Screen {
        int listing;
    }

    static final class ConfirmListing extends Screen {
        BigDecimal price;
        ItemStack inHand;
        int handSlot;
    }

    static final class ExpiredScreen extends Screen {}

    static final class SellScreen extends Screen {}

    // --- Opening -------------------------------------------------------------------------------

    void openAuction(Player player, int page) {
        AuctionScreen screen = new AuctionScreen();
        screen.inventory = Bukkit.createInventory(screen, 54, Component.text("Auction House"));
        screen.page = page;
        drawAuction(screen);
        player.openInventory(screen.inventory);
    }

    void openExpired(Player player) {
        ExpiredScreen screen = new ExpiredScreen();
        screen.inventory = Bukkit.createInventory(screen, 54, Component.text("Expired Listings"));
        drawExpired(screen, player);
        player.openInventory(screen.inventory);
    }

    void openSell(Player player) {
        SellScreen screen = new SellScreen();
        screen.inventory = Bukkit.createInventory(screen, 54, Component.text("Sell Items"));
        for (int slot = PAGE; slot < 54; slot++) {
            screen.inventory.setItem(slot, filler());
        }
        screen.inventory.setItem(49, button(Material.EMERALD, "Sell", NamedTextColor.GREEN,
                "Sells every item above that has a price"));
        player.openInventory(screen.inventory);
    }

    /** `/ah sell <price>`: the item in the player's hand, to be confirmed. */
    void openConfirmListing(Player player, BigDecimal price) {
        ItemStack hand = player.getInventory().getItemInMainHand();
        if (hand.getType().isAir()) {
            player.sendMessage(Component.text("Hold the item you want to sell.", NamedTextColor.RED));
            return;
        }
        ConfirmListing screen = new ConfirmListing();
        screen.inventory = Bukkit.createInventory(screen, 27, Component.text("Confirm Listing"));
        screen.price = price;
        screen.inHand = hand.clone();
        screen.handSlot = player.getInventory().getHeldItemSlot();
        drawConfirm(screen.inventory, withLore(hand, List.of(
                Component.text("Price: " + Market.money(price), NamedTextColor.GOLD))));
        player.openInventory(screen.inventory);
    }

    private void openConfirmPurchase(Player player, Market.Listing listing) {
        ConfirmPurchase screen = new ConfirmPurchase();
        screen.inventory = Bukkit.createInventory(screen, 27, Component.text("Confirm Purchase"));
        screen.listing = listing.id();
        drawConfirm(screen.inventory, shown(listing));
        player.openInventory(screen.inventory);
    }

    // --- Drawing -------------------------------------------------------------------------------

    private void drawAuction(AuctionScreen screen) {
        Inventory inventory = screen.inventory;
        inventory.clear();
        screen.listingAt.clear();
        List<Market.Listing> listings = market.listings();
        int pages = Math.max(1, (listings.size() + PAGE - 1) / PAGE);
        screen.page = Math.max(0, Math.min(screen.page, pages - 1));
        int first = screen.page * PAGE;
        for (int slot = 0; slot < PAGE && first + slot < listings.size(); slot++) {
            Market.Listing listing = listings.get(first + slot);
            inventory.setItem(slot, shown(listing));
            screen.listingAt.put(slot, listing.id());
        }
        for (int slot = PAGE; slot < 54; slot++) {
            inventory.setItem(slot, filler());
        }
        if (screen.page > 0) {
            inventory.setItem(45, button(Material.ARROW, "Previous page", NamedTextColor.YELLOW));
        }
        inventory.setItem(48, button(Material.PAPER, "Page " + (screen.page + 1) + " of " + pages,
                NamedTextColor.WHITE));
        Player viewer = viewer(inventory);
        int expired = viewer == null ? 0 : market.expired(viewer.getName()).size();
        inventory.setItem(49, button(Material.CHEST, "Expired listings", NamedTextColor.YELLOW,
                expired + " to collect"));
        inventory.setItem(50, button(Material.SUNFLOWER, "Refresh", NamedTextColor.YELLOW));
        if (screen.page < pages - 1) {
            inventory.setItem(53, button(Material.ARROW, "Next page", NamedTextColor.YELLOW));
        }
    }

    private void drawExpired(ExpiredScreen screen, Player player) {
        Inventory inventory = screen.inventory;
        inventory.clear();
        List<ItemStack> items = market.expired(player.getName());
        for (int slot = 0; slot < PAGE && slot < items.size(); slot++) {
            inventory.setItem(slot, withLore(items.get(slot), List.of(
                    Component.text("Click to collect", NamedTextColor.GRAY))));
        }
        for (int slot = PAGE; slot < 54; slot++) {
            inventory.setItem(slot, filler());
        }
        inventory.setItem(49, button(Material.ARROW, "Back", NamedTextColor.YELLOW));
    }

    private static void drawConfirm(Inventory inventory, ItemStack item) {
        for (int slot = 0; slot < 27; slot++) {
            inventory.setItem(slot, filler());
        }
        inventory.setItem(11, button(Material.LIME_STAINED_GLASS_PANE, "Confirm", NamedTextColor.GREEN));
        inventory.setItem(13, item);
        inventory.setItem(15, button(Material.RED_STAINED_GLASS_PANE, "Cancel", NamedTextColor.RED));
    }

    /** A listing as the auction house shows it: the item, and below its own lore the sale. */
    private ItemStack shown(Market.Listing listing) {
        long ticksLeft = Math.max(0, listing.expiresAtTick() - Bukkit.getCurrentTick());
        long minutes = ticksLeft / 20 / 60;
        long seconds = ticksLeft / 20 % 60;
        return withLore(listing.item(), List.of(
                Component.text("Price: " + Market.money(listing.price()), NamedTextColor.GOLD),
                Component.text("Seller: " + listing.seller(), NamedTextColor.GRAY),
                Component.text("Expires in: " + minutes + "m " + seconds + "s", NamedTextColor.GRAY)));
    }

    private static ItemStack withLore(ItemStack item, List<Component> added) {
        ItemStack copy = item.clone();
        ItemMeta meta = copy.getItemMeta();
        List<Component> lore = new ArrayList<>();
        if (meta.lore() != null) {
            lore.addAll(meta.lore());
        }
        for (Component line : added) {
            lore.add(line.decoration(TextDecoration.ITALIC, false));
        }
        meta.lore(lore);
        copy.setItemMeta(meta);
        return copy;
    }

    private static ItemStack button(Material material, String name, NamedTextColor color, String... lore) {
        ItemStack item = new ItemStack(material);
        ItemMeta meta = item.getItemMeta();
        meta.displayName(Component.text(name, color).decoration(TextDecoration.ITALIC, false));
        List<Component> lines = new ArrayList<>();
        for (String line : lore) {
            lines.add(Component.text(line, NamedTextColor.GRAY).decoration(TextDecoration.ITALIC, false));
        }
        if (!lines.isEmpty()) {
            meta.lore(lines);
        }
        item.setItemMeta(meta);
        return item;
    }

    private static ItemStack filler() {
        return button(Material.BLACK_STAINED_GLASS_PANE, " ", NamedTextColor.DARK_GRAY);
    }

    private static Player viewer(Inventory inventory) {
        return inventory.getViewers().isEmpty() ? null : (Player) inventory.getViewers().get(0);
    }

    // --- Clicks --------------------------------------------------------------------------------

    /** Whether `holder` is one of the harness's screens. */
    static boolean ours(InventoryHolder holder) {
        return holder instanceof Screen;
    }

    void onClick(InventoryClickEvent event) {
        InventoryHolder holder = event.getView().getTopInventory().getHolder();
        if (!(holder instanceof Screen) || !(event.getWhoClicked() instanceof Player player)) {
            return;
        }
        int raw = event.getRawSlot();
        boolean top = raw >= 0 && raw < event.getView().getTopInventory().getSize();
        if (holder instanceof SellScreen) {
            // Slots 0 to 44 and the player's own take items; the bottom row is buttons.
            if (top && raw >= PAGE) {
                event.setCancelled(true);
                if (raw == 49) {
                    sell(player, event.getView().getTopInventory());
                }
            }
            return;
        }
        event.setCancelled(true);
        if (!top) {
            return;
        }
        switch (holder) {
            case AuctionScreen screen -> clickAuction(player, screen, raw);
            case ConfirmPurchase screen -> clickConfirmPurchase(player, screen, raw);
            case ConfirmListing screen -> clickConfirmListing(player, screen, raw);
            case ExpiredScreen screen -> clickExpired(player, screen, raw);
            default -> {}
        }
    }

    void onDrag(InventoryDragEvent event) {
        InventoryHolder holder = event.getView().getTopInventory().getHolder();
        if (!(holder instanceof Screen)) {
            return;
        }
        int size = event.getView().getTopInventory().getSize();
        boolean intoButtons = event.getRawSlots().stream()
                .anyMatch(slot -> slot < size && (!(holder instanceof SellScreen) || slot >= PAGE));
        if (intoButtons) {
            event.setCancelled(true);
        }
    }

    void onClose(InventoryCloseEvent event) {
        if (event.getView().getTopInventory().getHolder() instanceof SellScreen
                && event.getPlayer() instanceof Player player) {
            Inventory top = event.getView().getTopInventory();
            for (int slot = 0; slot < PAGE; slot++) {
                ItemStack item = top.getItem(slot);
                if (item != null && !item.getType().isAir()) {
                    top.setItem(slot, null);
                    giveOrDrop(player, item);
                }
            }
        }
    }

    private void clickAuction(Player player, AuctionScreen screen, int slot) {
        Integer id = screen.listingAt.get(slot);
        if (id != null) {
            Optional<Market.Listing> listing = market.listing(id);
            if (listing.isEmpty()) {
                player.sendMessage(Component.text(Market.Refusal.GONE.text, NamedTextColor.RED));
                drawAuction(screen);
                return;
            }
            // As most plugins do: never open a screen inside the click's own event.
            Bukkit.getScheduler().runTask(plugin, () -> openConfirmPurchase(player, listing.get()));
            return;
        }
        switch (slot) {
            case 45 -> {
                screen.page--;
                drawAuction(screen);
            }
            case 53 -> {
                screen.page++;
                drawAuction(screen);
            }
            case 50 -> drawAuction(screen);
            case 49 -> Bukkit.getScheduler().runTask(plugin, () -> openExpired(player));
            default -> {}
        }
    }

    private void clickConfirmPurchase(Player player, ConfirmPurchase screen, int slot) {
        if (slot == 11) {
            plugin.purchase(player, screen.listing);
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        } else if (slot == 15) {
            Bukkit.getScheduler().runTask(plugin, () -> openAuction(player, 0));
        }
    }

    private void clickConfirmListing(Player player, ConfirmListing screen, int slot) {
        if (slot == 11) {
            ItemStack hand = player.getInventory().getItem(screen.handSlot);
            if (hand == null || !hand.isSimilar(screen.inHand) || hand.getAmount() != screen.inHand.getAmount()) {
                player.sendMessage(Component.text("The item in your hand changed.", NamedTextColor.RED));
            } else {
                player.getInventory().setItem(screen.handSlot, null);
                Market.Listing listing = market.list(player.getName(), screen.inHand, screen.price,
                        Bukkit.getCurrentTick());
                plugin.getLogger().info("harness-listed id=" + listing.id() + " seller=" + player.getName()
                        + " price=" + listing.price().toPlainString());
                player.sendMessage(Component.text("Listed for " + Market.money(screen.price) + ".",
                        NamedTextColor.GREEN));
            }
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        } else if (slot == 15) {
            Bukkit.getScheduler().runTask(plugin, () -> player.closeInventory());
        }
    }

    private void clickExpired(Player player, ExpiredScreen screen, int slot) {
        if (slot == 49) {
            Bukkit.getScheduler().runTask(plugin, () -> openAuction(player, 0));
            return;
        }
        List<ItemStack> items = market.expired(player.getName());
        if (slot >= items.size()) {
            return;
        }
        ItemStack item = items.get(slot);
        if (!player.getInventory().addItem(item.clone()).isEmpty()) {
            // addItem may have placed part of it: take that back.
            player.getInventory().removeItem(item.clone());
            player.sendMessage(Component.text(Market.Refusal.FULL.text, NamedTextColor.RED));
            return;
        }
        items.remove(slot);
        plugin.getLogger().info("harness-collected player=" + player.getName());
        drawExpired(screen, player);
    }

    private void sell(Player player, Inventory top) {
        BigDecimal total = BigDecimal.ZERO;
        int sold = 0;
        for (int slot = 0; slot < PAGE; slot++) {
            ItemStack item = top.getItem(slot);
            if (item == null || item.getType().isAir()) {
                continue;
            }
            Optional<BigDecimal> price = market.sellPrice(item);
            if (price.isPresent()) {
                total = total.add(price.get().multiply(BigDecimal.valueOf(item.getAmount())));
                sold += item.getAmount();
                top.setItem(slot, null);
            }
        }
        if (sold == 0) {
            player.sendMessage(Component.text("Nothing here sells.", NamedTextColor.RED));
            return;
        }
        market.setBalance(player.getName(), market.balance(player.getName()).add(total));
        plugin.getLogger().info("harness-sold player=" + player.getName() + " items=" + sold
                + " total=" + total.toPlainString());
        player.sendMessage(Component.text("Sold " + sold + " items for " + Market.money(total) + ".",
                NamedTextColor.GREEN));
    }

    static void giveOrDrop(Player player, ItemStack item) {
        for (ItemStack left : player.getInventory().addItem(item).values()) {
            player.getWorld().dropItem(player.getLocation(), left);
        }
    }
}
