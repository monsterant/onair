// 토스페이먼츠 결제위젯(v2) 불러오기와 결제 요청 (#41, 테스트 모드).
// SDK는 후원하기를 누를 때만 불러온다(다른 화면을 가볍게 하려는 것).
import type { Checkout } from './api';

type Widgets = {
  setAmount(a: { currency: 'KRW'; value: number }): Promise<void>;
  renderPaymentMethods(o: { selector: string; variantKey?: string }): Promise<unknown>;
  renderAgreement(o: { selector: string; variantKey?: string }): Promise<unknown>;
  requestPayment(o: { orderId: string; orderName: string; successUrl: string; failUrl: string }): Promise<void>;
};
type TossPaymentsFn = (clientKey: string) => { widgets(o: { customerKey: string }): Widgets };

const SDK = 'https://js.tosspayments.com/v2/standard';
let loading: Promise<TossPaymentsFn> | null = null;

function loadSdk(): Promise<TossPaymentsFn> {
  const w = window as unknown as { TossPayments?: TossPaymentsFn };
  if (w.TossPayments) return Promise.resolve(w.TossPayments);
  loading ??= new Promise((resolve, reject) => {
    const s = document.createElement('script');
    s.src = SDK;
    s.onload = () => {
      if (w.TossPayments) return resolve(w.TossPayments);
      // 실패를 기억하지 않는다: 다음에 누르면 다시 불러온다(#41 리뷰)
      loading = null;
      s.remove();
      reject(new Error('결제 모듈을 불러오지 못했습니다'));
    };
    s.onerror = () => {
      loading = null;
      s.remove();
      reject(new Error('결제 모듈을 불러오지 못했습니다'));
    };
    document.head.append(s);
  });
  return loading;
}

/**
 * 결제위젯을 그린다. 돌려준 pay()를 부르면 토스 결제창으로 넘어가고,
 * 끝나면 /donate/success(또는 fail)?b=<방송 id>&paymentKey=...&orderId=...&amount=... 로 돌아온다.
 */
export async function mountWidget(c: Checkout, broadcastId: number, methodsSel: string, agreementSel: string) {
  const TossPayments = await loadSdk();
  const widgets = TossPayments(c.client_key).widgets({ customerKey: c.customer_key });
  await widgets.setAmount({ currency: 'KRW', value: c.amount });
  await Promise.all([
    widgets.renderPaymentMethods({ selector: methodsSel, variantKey: 'DEFAULT' }),
    widgets.renderAgreement({ selector: agreementSel, variantKey: 'AGREEMENT' }),
  ]);
  const back = (kind: 'success' | 'fail') => `${location.origin}/donate/${kind}?b=${broadcastId}`;
  return {
    pay: () =>
      widgets.requestPayment({ orderId: c.order_id, orderName: c.order_name, successUrl: back('success'), failUrl: back('fail') }),
  };
}
