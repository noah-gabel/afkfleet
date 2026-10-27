// Home (Plan.md P8.6, P11.9; ADR-0020, U35): the bots' dashboard, right after the Header.
// Above the bots, the account's warnings and the links waiting for the user's approval
// (P9.8), one compact line each, only while they apply. Who is signed in is the Header's.
import { useMe } from "../../../api/queries";
import { WaitingAlert } from "../../accounts/components/WaitingAlert";
import { BotGrid } from "../../bots/components/BotGrid";
import { BotNotices } from "../../bots/components/BotNotices";
import { SecurityWarnings } from "../../profile/components/SecurityWarnings";

/** The first signed-in screen. */
export function Home() {
  const { data: user } = useMe();
  return (
    <main className="flex flex-col gap-6 p-6">
      {user === undefined ? null : (
        <div className="flex flex-col gap-2 empty:hidden">
          <SecurityWarnings user={user} place="home" />
          <WaitingAlert />
          <BotNotices />
        </div>
      )}
      <BotGrid />
    </main>
  );
}
